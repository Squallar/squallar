package com.rustdar;

import android.app.Activity;
import android.content.Context;
import android.hardware.Sensor;
import android.hardware.SensorEvent;
import android.hardware.SensorEventListener;
import android.hardware.SensorManager;

/**
 * Static helper that registers for rotation vector sensor updates and
 * provides the current compass heading (azimuth in degrees, 0–360).
 *
 * Loaded via PathClassLoader from native code (same pattern as BackHandler).
 */
public final class CompassHelper {
    private static volatile float sHeading = -1f;
    private static SensorEventListener sListener;
    private static SensorManager sSensorManager;

    /** Register for rotation vector sensor updates. Call once from android_main. */
    public static void register(Activity activity) {
        sSensorManager = (SensorManager) activity.getSystemService(Context.SENSOR_SERVICE);
        if (sSensorManager == null) return;

        Sensor rotation = sSensorManager.getDefaultSensor(Sensor.TYPE_ROTATION_VECTOR);
        if (rotation == null) return;

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

        sSensorManager.registerListener(
                sListener, rotation, SensorManager.SENSOR_DELAY_UI);
    }

    /** Unregister the sensor listener. */
    public static void unregister() {
        if (sSensorManager != null && sListener != null) {
            sSensorManager.unregisterListener(sListener);
        }
    }

    /**
     * Get the current compass heading in degrees (0–360).
     * Returns -1 if no reading is available yet.
     */
    public static float getHeading() {
        return sHeading;
    }
}
