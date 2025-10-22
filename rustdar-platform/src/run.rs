use winit::event_loop::{ControlFlow, EventLoop};

/// Create and configure a new event loop
fn create_event_loop() -> EventLoop<()> {
    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Poll);
    event_loop
}

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logger for native platforms
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    log::info!("Starting rustdar-platform (native)");

    let event_loop = create_event_loop();
    let mut app = crate::app::App::new();
    event_loop
        .run_app(&mut app)
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
}
