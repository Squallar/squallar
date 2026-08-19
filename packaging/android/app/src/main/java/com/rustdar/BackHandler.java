package com.rustdar;

import android.app.Activity;
import android.util.Log;

/**
 * Back-navigation callback for NativeActivity.
 *
 * <h2>Two dispatch routes, one decision</h2>
 *
 * Back reaches this app one of two ways, and which one is live is decided by
 * the app's opt-in, not by the device:
 *
 * <ul>
 *   <li><b>Legacy.</b> Back arrives as KEYCODE_BACK through the native input
 *       queue and is handled in Rust: InputHandler::take_back_out_press, then
 *       App::back_out, then App::resolve_back_press, which closes the topmost
 *       open layer and only minimises a press with nothing open.</li>
 *   <li><b>OnBackInvokedDispatcher.</b> Back bypasses the input queue and is
 *       handed to the callback registered below. With no callback registered,
 *       NativeActivity calls finish() and the app is destroyed, leaving a white
 *       box in recents.</li>
 * </ul>
 *
 * The callback below decides nothing. It hands the press to
 * {@link #nativeBackPressed()} — {@code Java_com_rustdar_BackHandler_nativeBackPressed}
 * in rustdar-android/src/lib.rs — which parks it and wakes the winit loop, and
 * that loop feeds it to the same App::resolve_back_press the legacy route ends
 * in. So both routes close one layer per press and minimise only when nothing
 * is open, and the minimise itself is the Rust side's moveTaskToBack.
 *
 * <h2>Which route is live today</h2>
 *
 * The platform only invokes registered callbacks when the app has opted in via
 * android:enableOnBackInvokedCallback. This app's manifest does not set it and
 * targetSdk is 34, so the registration below is inert and the legacy route is
 * the live one. Raising targetSdk to 35 or beyond opts the app in whether the
 * manifest says so or not, and from 36 there is no opting out.
 *
 * That switch is why this class must not decide anything. It used to minimise
 * unconditionally with no route into Rust, so the day targetSdk moved, one
 * press with the drawer open would have minimised the app again — silently, with
 * no test failing and nothing logged. Nothing about the routing below depends on
 * targetSdk, so the flip is now a non-event.
 *
 * The two routes are mutually exclusive: opting in is precisely what stops
 * KEYCODE_BACK being delivered, so a press is never dispatched both ways.
 */
public class BackHandler {

    private static final String TAG = "rustdar";

    /**
     * Register the back-navigation callback.
     *
     * The SDK_INT guard is about whether the API <em>exists</em> to call, not
     * about whether the callback will fire: that is the manifest opt-in
     * described above. Safe to call on any API level.
     */
    public static void register(Activity activity) {
        if (android.os.Build.VERSION.SDK_INT >= 33) {
            registerBackCallback(activity);
        }
    }

    @android.annotation.TargetApi(33)
    private static void registerBackCallback(Activity activity) {
        activity.getOnBackInvokedDispatcher().registerOnBackInvokedCallback(
            android.window.OnBackInvokedDispatcher.PRIORITY_DEFAULT,
            () -> onBackInvoked(activity)
        );
    }

    /**
     * Route one press into Rust, or minimise if it cannot be routed.
     *
     * onBackInvoked() is {@code void}, so "the app declined this press" cannot
     * be expressed as a return value and the alternatives — unregistering and
     * re-invoking, or registering only while something is open — would put the
     * decision back on this side of the JNI boundary, which is the bug. Instead
     * the app always consumes the press and performs its own minimise from
     * Rust, which is exactly what the legacy route does.
     *
     * The one case this side decides is when there is no Rust to route to:
     * android_main installs the event-loop proxy only after this class has been
     * registered, and a press in that window returns false here. Falling back to
     * moveTaskToBack keeps the pre-existing behaviour as the failure mode rather
     * than swallowing the gesture. Throwable, not Exception: a missing native
     * symbol is an UnsatisfiedLinkError.
     */
    private static void onBackInvoked(Activity activity) {
        boolean routed;
        try {
            routed = nativeBackPressed();
        } catch (Throwable t) {
            Log.e(TAG, "nativeBackPressed() is unreachable; minimising instead", t);
            routed = false;
        }
        if (!routed) {
            activity.moveTaskToBack(true);
        }
    }

    /**
     * Park a back press for the winit event loop and wake it.
     *
     * Returns false when there is no event loop to hand it to. Implemented in
     * librustdar_android.so, which NativeActivity has already loaded through
     * this same ClassLoader by the time android_main registers this class.
     */
    private static native boolean nativeBackPressed();
}
