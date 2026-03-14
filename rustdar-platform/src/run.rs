use winit::event_loop::{ControlFlow, EventLoop};

/// Create and configure a new event loop
fn create_event_loop() -> EventLoop<()> {
    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Wait);
    event_loop
}

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logger for native platforms
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    // Suppress winit EventLoopClosed panics from background threads on exit.
    // catch_unwind prevents crashes but the default hook still prints to stderr.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info.to_string();
        if !msg.contains("EventLoopClosed") {
            default_hook(info);
        }
    }));

    log::info!("Starting rustdar-platform (native)");

    let event_loop = create_event_loop();
    let mut app = crate::app::App::new();
    event_loop
        .run_app(&mut app)
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
}
