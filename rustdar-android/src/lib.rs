//! Android entry point for Rustdar Platform
//! 
//! This crate provides the Android-specific entry point and configuration
//! for the Rustdar radar visualization application.

#[cfg(target_os = "android")]
use winit::platform::android::activity::AndroidApp;

/// Android main entry point
/// 
/// This function is called by the Android runtime when the app starts.
/// It initializes logging and starts the main application loop.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    use winit::platform::android::EventLoopBuilderExtAndroid;
    
    // Initialize Android logging
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("rustdar"),
    );

    log::info!("Starting Rustdar Platform (Android)");

    // Create event loop with Android app
    let event_loop = winit::event_loop::EventLoop::builder()
        .with_android_app(app)
        .build()
        .expect("Failed to create event loop");

    // Create and run the platform app
    let mut platform_app = rustdar_platform_lib::app::App::new();
    
    if let Err(e) = event_loop.run_app(&mut platform_app) {
        log::error!("Application error: {}", e);
    }
}

// For non-Android targets, this crate does nothing
#[cfg(not(target_os = "android"))]
pub fn _unused() {
    // This function exists to make the crate compile on non-Android targets
    // It will never be called
}