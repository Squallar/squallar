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

    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    // Swallow the EventLoopClosed panics background threads raise on exit.
    // Matched on the payload, not a stringified PanicInfo; both &str and String
    // because `panic!()` produces &str for literals and String for formatted.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
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

    log::info!("Starting squallar (native); malloc arenas: {arenas:?}");

    // Before the app, because the first alerts round is what consumes it.
    // Only names the URL; nothing is fetched here.
    crate::platform::name_the_zone_pack();

    let event_loop = create_event_loop();
    let mut app = squallar_app::app::App::new(
        Box::new(crate::platform::create_platform()),
        crate::platform::create_location(),
    );
    event_loop
        .run_app(&mut app)
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
}
