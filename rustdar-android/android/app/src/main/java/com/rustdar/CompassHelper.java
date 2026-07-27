package com.rustdar;

import android.app.Activity;
import android.app.Application;
import android.content.Context;
import android.hardware.Sensor;
import android.hardware.SensorEvent;
import android.hardware.SensorEventListener;
import android.hardware.SensorManager;
import android.os.Bundle;

/**
 * Static helper that registers for rotation vector sensor updates and
 * provides the current compass heading (azimuth in degrees, 0–360).
 *
 * Loaded by name through the app ClassLoader from android_main; see
 * register_java_helper in rustdar-android/src/lib.rs.
 *
 * <h2>Why this follows the Activity lifecycle</h2>
 *
 * The listener used to be registered once from {@link #register} and never
 * unregistered: {@code unregister()} was public, kept alive by an explicit R8
 * rule, and called from nowhere. TYPE_ROTATION_VECTOR is a fused sensor, so
 * that left the accelerometer, gyroscope and magnetometer being sampled at
 * SENSOR_DELAY_UI for the entire life of the process — including after the back
 * button minimised the app, when nothing is drawing the compass and the Rust
 * polling thread's readings go nowhere.
 *
 * Driving it from ActivityLifecycleCallbacks keeps the sensor on exactly while
 * the Activity is resumed. That is also the pairing R8 can see, which is why
 * proguard-rules.pro no longer has to name {@code unregister} to keep it.
 */
public final class CompassHelper {
    // All of this state is written from one thread and read from another, so
    // none of it can be a plain static.
    //
    // `register` runs on the android_main thread (called over JNI). The
    // lifecycle callbacks and the sensor callbacks run on the UI thread.
    // `getHeading` is called over JNI from the compass polling thread. Without
    // volatile there is no happens-before edge between the write in `register`
    // and the reads in `startListening`/`stopListening` on the UI thread, so
    // the listener fields can be seen as null there and the `sListening` guard
    // is not reliable — which would let a pause/resume double-register the
    // listener, or unregister nothing.
    //
    // Volatile alone used to be argued sufficient on the grounds that every
    // field was "written once by `register` before the callbacks are
    // registered". That premise died when register() stopped being
    // once-per-process: android_main runs once per Activity *instance*, so a
    // second register() rewrites these fields from its own thread while the
    // process-wide lifecycle callbacks may be flipping the listener on the UI
    // thread. The register/startListening/stopListening transitions are
    // therefore serialised on the class lock (`static synchronized`); volatile
    // stays for the lock-free readers -- `getHeading` on the poll thread, and
    // the sensor callback's `sHeading` write.
    private static volatile float sHeading = -1f;
    private static volatile SensorEventListener sListener;
    private static volatile SensorManager sSensorManager;
    private static volatile Sensor sRotationSensor;
    /** Whether the listener is currently registered. Guarded by the class lock. */
    private static volatile boolean sListening;
    /**
     * Whether the lifecycle callbacks are installed. They are registered once
     * per *process* -- they hold no per-Activity state -- against register()'s
     * once-per-Activity call cadence; without the guard, each new Activity
     * stacked another never-unregistered callback.
     */
    private static volatile boolean sCallbacksRegistered;

    /**
     * Register for rotation vector sensor updates. Called once per
     * android_main — which is once per Activity <em>instance</em>, not once
     * per process, so this must be idempotent: see the guards below.
     */
    public static synchronized void register(Activity activity) {
        // A second Activity means a second android_main and a second call
        // here. If the previous listener is still registered — the new
        // Activity can resume before this runs, and that onActivityResumed
        // re-registers whatever sListener holds — unregister it now, before
        // the overwrites below orphan it into exactly the everlasting sensor
        // drain the class comment says was fixed.
        stopListening();

        sSensorManager = (SensorManager) activity.getSystemService(Context.SENSOR_SERVICE);
        if (sSensorManager == null) return;

        sRotationSensor = sSensorManager.getDefaultSensor(Sensor.TYPE_ROTATION_VECTOR);
        if (sRotationSensor == null) return;

        // Anonymous class of a *static* method, so it holds no reference to the
        // Activity. Neither does SensorManager, which is a process-scoped system
        // service. Nothing here keeps the Activity alive.
        sListener = new SensorEventListener() {
            private final float[] rotationMatrix = new float[9];
            private final float[] orientation = new float[3];

            @Override
            public void onSensorChanged(SensorEvent event) {
                SensorManager.getRotationMatrixFromVector(rotationMatrix, event.values);
                SensorManager.getOrientation(rotationMatrix, orientation);
                // orientation[0] is azimuth in radians (-π to π) → convert to degrees (0–360)
                float azimuthDeg = (float) Math.toDegrees(orientation[0]);
                if (azimuthDeg < 0) azimuthDeg += 360f;
                sHeading = azimuthDeg;
            }

            @Override
            public void onAccuracyChanged(Sensor sensor, int accuracy) {
                // ignored
            }
        };

        startListening();

        // register() is called from android_main, which runs after onCreate and
        // therefore after the onActivityResumed for this Activity has already
        // gone by. Hence the startListening() above: the callbacks below take
        // over from the first pause onwards.
        //
        // The app declares a single Activity (android.app.NativeActivity, in a
        // singleTask task), so there is no need to match on identity here —
        // and not holding an Activity reference is what keeps this leak-free.
        // It is also what lets one registration serve every later Activity
        // instance, which is why sCallbacksRegistered gates it rather than
        // this stacking a fresh callback per register() call.
        Application app = activity.getApplication();
        if (app != null && !sCallbacksRegistered) {
            sCallbacksRegistered = true;
            app.registerActivityLifecycleCallbacks(new Application.ActivityLifecycleCallbacks() {
                @Override
                public void onActivityResumed(Activity a) {
                    startListening();
                }

                @Override
                public void onActivityPaused(Activity a) {
                    stopListening();
                }

                @Override public void onActivityCreated(Activity a, Bundle b) { }
                @Override public void onActivityStarted(Activity a) { }
                @Override public void onActivityStopped(Activity a) { }
                @Override public void onActivitySaveInstanceState(Activity a, Bundle b) { }
                @Override public void onActivityDestroyed(Activity a) { }
            });
        }
    }

    private static synchronized void startListening() {
        if (sListening || sSensorManager == null || sListener == null || sRotationSensor == null) {
            return;
        }
        sSensorManager.registerListener(sListener, sRotationSensor, SensorManager.SENSOR_DELAY_UI);
        sListening = true;
    }

    private static synchronized void stopListening() {
        if (!sListening || sSensorManager == null || sListener == null) return;
        sSensorManager.unregisterListener(sListener);
        sListening = false;
        // Drop the stale reading rather than leaving the last one to be served
        // as current for however long the app stays backgrounded.
        sHeading = -1f;
    }

    /**
     * Get the current compass heading in degrees (0–360).
     * Returns -1 if no reading is available yet, or while the app is paused.
     *
     * Called over JNI from get_compass_heading(); kept by name in
     * proguard-rules.pro.
     */
    public static float getHeading() {
        return sHeading;
    }
}
