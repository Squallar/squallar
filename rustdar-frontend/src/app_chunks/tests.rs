/// The chunk drain and driver must run inside `poll_data_channels`, which
/// `handle_redraw` calls before `evict_unshown_scans` and before
/// `setup_egui_frame` lays the frame out.
///
/// A source probe because no type system expresses it: the drain is what
/// makes a newly assembled volume the one `dispatch_pane_renders` reads, and
/// `evict_unshown_scans` would drop a volume stored after it ran. The
/// sibling guarantee for the pollers inside `setup_egui_frame` is pinned by
/// `app_render::tests::every_poller_runs_before_the_frame_is_laid_out`; this
/// is the half that lives outside it.
#[test]
fn the_chunk_drain_runs_before_the_frame_is_laid_out() {
    let source = include_str!("../app.rs");
    let body = |name: &str| {
        let start = source
            .find(name)
            .unwrap_or_else(|| panic!("{name} is gone from app.rs"));
        let rest = &source[start..];
        let open = rest.find('{').expect("a body");
        let mut depth = 0usize;
        for (i, c) in rest[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return rest[open..open + i].to_string();
                    }
                }
                _ => {}
            }
        }
        panic!("unbalanced braces in {name}");
    };

    let poll = body("fn poll_data_channels(");
    let drain = poll.find("self.poll_chunk_results(").expect(
        "the chunk drain left poll_data_channels, so a volume it \
                     assembles can be evicted before anything draws it",
    );
    let drive = poll
        .find("self.drive_chunk_feeds(")
        .expect("the chunk driver left poll_data_channels");
    assert!(
        drain < drive,
        "a round is dispatched before the finished one is applied, so every \
             volume waits an extra frame"
    );

    let redraw = body("fn handle_redraw(");
    let at = |needle: &str| {
        redraw
            .find(needle)
            .unwrap_or_else(|| panic!("{needle} is gone from handle_redraw"))
    };
    assert!(
        at("self.poll_data_channels(") < at("self.evict_unshown_scans("),
        "a volume the chunk drain stores is evicted in the same frame"
    );
    assert!(
        at("self.poll_data_channels(") < at("self.setup_egui_frame("),
        "the frame is laid out before the chunk drain has applied anything"
    );
}

/// Reconnection must not be conditional on anything else being busy.
///
/// Three source probes, because all of it is positional and none of it has a
/// type that could carry the requirement. `sync_sites` is the only thing that
/// reopens a dropped socket and it only runs on a frame, so the frame has to
/// keep coming while a reconnect is owed — and the notification driver has to
/// sit ahead of the `enabled` gate, or turning the chunk feed off would
/// strand the socket rather than narrowing it to the archive feed.
///
/// The reconnect is owed in two different ways and each needs its own route.
/// A handshake resolves or times out within `CONNECT_TIMEOUT`, so the re-arm
/// carries it; a backoff doubles to a five-minute ceiling and never gives up,
/// so the *schedule* carries it. Putting the backoff back in the re-arm is
/// what this catches, and it is not a hypothetical: that is where it was, and
/// for anyone who cannot reach the notifier it drew at refresh rate for the
/// whole session.
#[test]
fn a_down_socket_is_retried_regardless_of_other_activity() {
    let redraw = include_str!("../app.rs");
    let arm = redraw
        .find("fn handle_redraw(")
        .map(|i| &redraw[i..])
        .expect("handle_redraw is gone from app.rs");
    let first_redraw = arm
        .find("notify_redraw(&self.window)")
        .unwrap_or(usize::MAX);
    assert!(
        arm.find("self.chunk_notify.handshake_pending()")
            .is_some_and(|at| at < first_redraw),
        "the re-arm dropped its handshake term, so a socket that goes down \
             with auto-poll off is never retried"
    );
    assert!(
        !arm[..first_redraw.min(arm.len())].contains("self.chunk_notify.next_retry_delay()"),
        "the notifier's backoff is back in the unconditional re-arm, which is \
             a permanent spinner for anyone who cannot reach the service: it \
             retries for the life of the session by design"
    );
    let fold = redraw
        .find("fn auto_poll_delay(")
        .map(|i| &redraw[i..])
        .expect("auto_poll_delay is gone from app.rs");
    assert!(
        fold.find("self.chunk_notify.next_retry_delay()")
            .is_some_and(|at| at < fold.find("\n    }").unwrap_or(usize::MAX)),
        "the backoff is neither re-armed on nor scheduled for, so a dropped \
             socket is retried only if something unrelated draws a frame"
    );

    let chunks = include_str!("../app_chunks.rs");
    let drive = chunks
        .find("fn drive_chunk_feeds(")
        .map(|i| &chunks[i..])
        .expect("drive_chunk_feeds is gone");
    let notify = drive
        .find("self.drive_chunk_notifications(")
        .expect("the notification driver left drive_chunk_feeds");
    let gate = drive
        .find("if !enabled {")
        .expect("the enabled gate left drive_chunk_feeds");
    assert!(
        notify < gate,
        "notifications are driven behind the live-chunk gate, so turning the \
             feed off drops the archive socket and stops reconnecting"
    );
}
