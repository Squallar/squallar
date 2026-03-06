package com.rustdar;

import android.app.Activity;

/**
 * Handles the Android system back gesture / button for NativeActivity.
 *
 * On Android 13+ (API 33), back gestures bypass the native input queue entirely
 * and go through OnBackInvokedDispatcher. Without registering a callback,
 * NativeActivity calls finish() and the app is destroyed (leaving a white box
 * in recents). This class registers a callback that minimises the app instead.
 *
 * On Android <13, back events arrive as KEYCODE_BACK through the input queue
 * and are handled on the Rust side.
 */
public class BackHandler {

    /**
     * Register a back-navigation callback that moves the task to background
     * instead of finishing the Activity.
     *
     * Safe to call on any API level — the OnBackInvokedCallback is only
     * registered when running on Android 13+ (API 33).
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
