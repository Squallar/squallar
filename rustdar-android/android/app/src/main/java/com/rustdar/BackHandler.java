package com.rustdar;

import android.app.Activity;

/**
 * Standby back-navigation callback for NativeActivity.
 *
 * Two dispatch paths exist, and which one is live is decided by the app's
 * opt-in, not by the device:
 *
 *  - Legacy: back arrives as KEYCODE_BACK through the native input queue and is
 *    handled in Rust — InputHandler::take_back_out_press, then App::back_out,
 *    which closes the topmost open layer and only minimises a press with
 *    nothing open.
 *  - OnBackInvokedDispatcher (API 33+): back bypasses the input queue. Without
 *    a registered callback NativeActivity calls finish() and the app is
 *    destroyed, leaving a white box in recents. The callback below minimises
 *    instead.
 *
 * The platform only invokes registered callbacks when the app has opted in via
 * android:enableOnBackInvokedCallback. This app's manifest does not set it and
 * targetSdk is 34, so the registration below is inert and the legacy path is
 * the live one — which is why the Rust rule applies today.
 *
 * That makes this class a safety net rather than the live handler, and it is
 * not equivalent to the Rust path: it minimises unconditionally and has no
 * route into Rust, so if it ever becomes live the UI never sees the press and
 * one press with the drawer open minimises again. Opting in — or raising
 * targetSdk to a level that opts in for you — therefore needs a native callback
 * here that asks Rust whether the press was consumed before minimising.
 */
public class BackHandler {

    /**
     * Register the standby back-navigation callback.
     *
     * The SDK_INT guard is about whether the API *exists* to call, not about
     * whether the callback will fire: that is the manifest opt-in described
     * above, which is off. Safe to call on any API level.
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
            () -> activity.moveTaskToBack(true)
        );
    }
}
