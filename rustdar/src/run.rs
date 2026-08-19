use winit::event_loop::{ControlFlow, EventLoop};

fn create_event_loop() -> EventLoop<()> {
    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Wait);
    event_loop
}

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Pin the rustls provider at a predictable point rather than letting
    // whichever background task fetches first choose it. Redundant — every
    // client constructor calls this too (see `rustdar_radar::tls`).
    rustdar_app::tls::init();

    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    // Swallow the EventLoopClosed panics background threads raise on exit.
    // Matched on the payload, not a stringified PanicInfo (whose paths and line
    // numbers shift across Rust versions); both &str and String because
    // `panic!()` produces &str for literals and String for formatted messages.
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

    log::info!("Starting rustdar (native)");

    let event_loop = create_event_loop();
    let mut app = rustdar_app::app::App::new(
        Box::new(crate::platform::create_platform()),
        crate::platform::create_location(),
    );
    event_loop
        .run_app(&mut app)
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
}
