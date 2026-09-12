use winit::event_loop::{ControlFlow, EventLoop};

fn create_event_loop() -> EventLoop<()> {
    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Wait);
    event_loop
}

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // **First statement, and that is the whole contract.** `M_ARENA_MAX` bounds
    // the arenas glibc may open from the moment it is set; ones already created
    // stay created. `main` reaches here through `pollster::block_on`, so no
    // runtime and no worker thread exists yet — after the tokio and async-std
    // pools are up this call would be capping a count already reached.
    // Reported rather than assumed: on a non-glibc target it answers `NotGlibc`
    // and changes nothing.
    let arenas = crate::arenas::cap_malloc_arenas();

    // Pin the rustls provider at a predictable point rather than letting
    // whichever background task fetches first choose it. Redundant.
    squallar_app::tls::init();

    // **The write goes to a thread of its own.** The 2 s telemetry tick says
    // 115 records in one frame, and with a plain `Target::Stderr` every one of
    // them is a blocking `write(2)` on the frame thread — so that frame costs
    // whatever the reader on the other end of fd 2 takes to drain 34 KB, which
    // measured 17.2 ms against a 2 MB/s consumer. See `crate::log_sink`.
    crate::log_sink::install(
        env_logger::Builder::from_default_env().filter_level(log::LevelFilter::Info),
    )
    .init();

    // Swallow the EventLoopClosed panics background threads raise on exit.
    // Matched on the payload, not a stringified PanicInfo; both &str and String
    // because `panic!()` produces &str for literals and String for formatted.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Before the hook prints: the panic message goes straight to fd 2
        // while log records are still queued behind `log_sink`'s writer
        // thread, and a reader would otherwise see the panic ahead of the
        // lines that led to it. Short, because a panicking process is already
        // leaving.
        let _ = crate::log_sink::drain(std::time::Duration::from_millis(500));
        let is_event_loop_closed = info
            .payload()
            .downcast_ref::<&str>()
            .is_some_and(|s| s.contains("EventLoopClosed"))
            || info
                .payload()
                .downcast_ref::<String>()
                .is_some_and(|s| s.contains("EventLoopClosed"));
        if !is_event_loop_closed {
            default_hook(info);
        }
    }));

    // Read on the thread `run_app` below will drive the event loop on, and
    // after the logger so it is visible. Nothing is set. A run launched
    // outside LaunchServices -- a terminal, a `cargo run`, a headless leg over
    // ssh -- is QoS-ceilinged and sits far more on the efficiency cores than
    // the bundle a user opens, so its timings do not compare; printing this
    // every launch is what stops that going unnoticed. See the module docs.
    let posture = crate::launch_posture::read();

    log::info!("Starting squallar (native); malloc arenas: {arenas:?}; launch posture: {posture}");

    // Before the app, because the first alerts round is what consumes it.
    // Only names the URL; nothing is fetched here.
    crate::platform::name_the_zone_pack();

    let event_loop = create_event_loop();
    let mut app = squallar_app::app::App::new(
        Box::new(crate::platform::create_platform()),
        crate::platform::create_location(),
    );
    // Somewhere for an archive the byte ceiling would otherwise drop. Taking
    // an archive strands the decoded volume in front of it — a median 15.5x
    // larger — as permanently un-evictable, because both decoded-eviction
    // policies refuse a volume with no way back; spilling keeps the way back
    // and still gives the heap bytes up.
    if let Some(root) = crate::platform::default_archive_spill_dir() {
        app.install_archive_spill(root);
    }
    let outcome = event_loop
        .run_app(&mut app)
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>);

    // **Nothing queued is lost on the way out.** The writer thread is not
    // joined at exit, so a process that leaves without waiting takes whatever
    // is still in the queue with it. Bounded rather than unbounded so a
    // wedged consumer delays the exit by a known amount instead of hanging it;
    // `log_sink::sink_line` is what says whether it emptied.
    let emptied = crate::log_sink::drain(std::time::Duration::from_secs(2));
    // **Said on every run, all-zero included.** A cut whose precondition never
    // holds delivers exactly nothing, and the only reading that says whether
    // this one fired is its own counter. `eprintln!` and not `log::info!`:
    // the queue has just been drained and this is the last thing the process
    // says. Sensitive to nothing but itself.
    if let Some(line) = crate::log_sink::sink_line() {
        let tail = if emptied {
            ""
        } else {
            "; queue not empty at the exit deadline"
        };
        eprintln!("{line}{tail}");
    }
    outcome
}
