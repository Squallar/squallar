//! Compass heading via JNI (CompassHelper.java): the class handle and the
//! 200 ms poll thread.

use super::with_env;

/// JClass for com.rustdar.CompassHelper, loaded once via the app class loader.
///
/// jni 0.22: `Global` is generic over the Java type it references, so this keeps
/// its `JClass`-ness and no longer needs an unsafe re-wrap to call statics on it.
pub(super) static COMPASS_CLASS: std::sync::OnceLock<
    jni::objects::Global<jni::objects::JClass<'static>>,
> = std::sync::OnceLock::new();

/// Read the current compass heading from CompassHelper.getHeading().
/// Returns `None` if the class wasn't loaded or no reading is available yet.
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
///
/// `wake` is the same handle, for the same reason, as
/// [`start_location_thread`]'s -- with one extra consequence at this cadence:
/// the heading is read five times a second, and the reading moves whenever the
/// device is held in a human hand, so this is the producer that would most
/// often be asking for a frame nothing else would have drawn. That is the
/// feature working (a heading-up map that does not turn is broken), and it is
/// bounded by the poll interval either way.
pub(super) fn start_compass_thread(
    sender: std::sync::mpsc::Sender<f32>,
    wake: impl Fn() + Send + 'static,
) {
    std::thread::Builder::new()
        .name("compass-heading".into())
        .spawn(move || {
            // Wait for CompassHelper to be initialized
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
