use winit::event_loop::{ControlFlow, EventLoop};

/// Create and configure a new event loop
fn create_event_loop() -> EventLoop<()> {
    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Poll);
    event_loop
}

#[cfg(not(target_arch = "wasm32"))]
fn run_native() -> Result<(), Box<dyn std::error::Error>> {
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

#[cfg(target_arch = "wasm32")]
async fn run_wasm() {
    let event_loop = create_event_loop();
    let mut app = crate::app::App::new();

    if let Err(e) = event_loop.run_app(&mut app) {
        log::error!("Event loop error: {:?}", e);
    }
}

#[cfg(target_arch = "wasm32")]
fn init_wasm_environment() {
    std::panic::set_hook(Box::new(console_error_panic_hook::hook));
    // Only initialize logger if it hasn't been initialized yet
    let _ = console_log::init_with_level(log::Level::Trace);
}

pub async fn run() {
    #[cfg(target_arch = "wasm32")]
    {
        init_wasm_environment();
        run_wasm().await;
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        match run_native() {
            Ok(_) => {}
            Err(e) => log::error!("Error: {}", e),
        }
    }
}
