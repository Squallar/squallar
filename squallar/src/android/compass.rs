//! Compass heading via JNI (`CompassHelper.kt`): the class handle and the
//! 200 ms poll thread.
//!
//! The sensor itself is registered and unregistered on the Kotlin side, against
//! the Activity's resumed window — see the class doc there. This side only
//! reads, and a paused app answers `-1` until it resumes.

use super::with_env;

/// JClass for app.squallar.CompassHelper, loaded once via the app class loader.
/// jni 0.22: `Global` is generic over the Java type it references, so this
/// keeps its `JClass`-ness.
pub(super) static COMPASS_CLASS: std::sync::OnceLock<
    jni::objects::Global<jni::objects::JClass<'static>>,
> = std::sync::OnceLock::new();

fn get_compass_heading() -> Option<f32> {
    use jni::objects::JClass;
    use jni::{jni_sig, jni_str};

    let global_ref = COMPASS_CLASS.get()?;

    let heading = with_env(|env| {
        let cls: &JClass<'static> = global_ref;
        env.call_static_method(cls, jni_str!("getHeading"), jni_sig!("()F"), &[])
            .and_then(|v| v.f())
            .ok()
    })
    .flatten()?;

    if heading < 0.0 {
        None // -1 means no reading yet
    } else {
        Some(heading)
    }
}

/// Start a background thread that polls the compass heading every 200ms and
/// sends updates through the provided channel.
/// `wake` is the same handle as [`start_location_thread`]'s. At five reads a
/// second, and a reading that moves whenever the device is held in a hand,
/// this is the producer most often asking for a frame nothing else would have
/// drawn — that is the feature working, and it is bounded by the poll interval.
pub(super) fn start_compass_thread(
    sender: std::sync::mpsc::Sender<f32>,
    wake: impl Fn() + Send + 'static,
) {
    std::thread::Builder::new()
        .name("compass-heading".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(4));

            loop {
                if let Some(heading) = get_compass_heading() {
                    if sender.send(heading).is_err() {
                        break; // channel closed
                    }
                    wake();
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        })
        .expect("failed to spawn compass-heading thread");
}
