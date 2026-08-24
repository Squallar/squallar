package app.squallar

import android.app.Activity
import android.app.Application
import android.content.Context
import android.hardware.Sensor
import android.hardware.SensorEvent
import android.hardware.SensorEventListener
import android.hardware.SensorManager
import android.hardware.display.DisplayManager
import android.os.Build
import android.os.Bundle
import android.view.Display
import android.view.Surface

/**
 * Static helper that registers for rotation vector sensor updates and provides
 * the current compass heading (azimuth in degrees, 0–360, relative to whatever
 * is currently "up" on screen — see the remap in the listener).
 *
 * Loaded by name through the app ClassLoader from android_main; see
 * register_java_helper in squallar/src/android/mod.rs. Read every 200 ms over JNI
 * by the compass-heading thread in squallar/src/android/compass.rs.
 *
 * ## Why this follows the Activity lifecycle
 *
 * The listener used to be registered once from [register] and never
 * unregistered. TYPE_ROTATION_VECTOR is a fused sensor, so that left the
 * accelerometer, gyroscope and magnetometer being sampled at SENSOR_DELAY_UI for
 * the entire life of the process — including after the app was minimised, when
 * nothing is drawing the compass and the Rust polling thread's readings go
 * nowhere.
 *
 * Driving it from ActivityLifecycleCallbacks keeps the sensor on exactly while
 * the Activity is resumed. `dumpsys sensorservice` showing no rotation-vector
 * connection for this package after backgrounding is the check that the fix is
 * still in place.
 *
 * ## Threading
 *
 * [register] runs on the android_main thread (called over JNI). The lifecycle
 * and sensor callbacks run on the UI thread. [getHeading] is called over JNI
 * from the compass polling thread. So none of this state can be a plain field:
 * the register/startListening/stopListening transitions are serialised on the
 * object monitor (`@Synchronized`), and `@Volatile` stays for the lock-free
 * readers — [getHeading] on the poll thread, and the sensor callback's heading
 * write and display read.
 *
 * Volatile alone was once argued sufficient on the grounds that every field is
 * "written once by register() before the callbacks are registered". That premise
 * died when register() stopped being once-per-process: android_main runs once
 * per Activity *instance*, so a second register() rewrites these fields from its
 * own thread while the process-wide lifecycle callbacks may be flipping the
 * listener on the UI thread.
 */
object CompassHelper {

    @Volatile
    private var headingDeg: Float = -1f

    @Volatile
    private var listener: SensorEventListener? = null

    @Volatile
    private var sensorManager: SensorManager? = null

    @Volatile
    private var rotationSensor: Sensor? = null

    /** Whether the listener is currently registered. Guarded by the monitor. */
    @Volatile
    private var listening: Boolean = false

    /**
     * Whether the lifecycle callbacks are installed. They are registered once
     * per *process* — they hold no per-Activity state — against register()'s
     * once-per-Activity call cadence; without the guard, each new Activity
     * stacked another never-unregistered callback.
     */
    @Volatile
    private var callbacksRegistered: Boolean = false

    /**
     * The display the Activity is on; its rotation feeds the coordinate remap in
     * the sensor callback. Held via DisplayManager off the application context,
     * never via the Activity — see the comment in [register].
     */
    @Volatile
    private var display: Display? = null

    // One listener exists at a time, so its scratch buffers live here rather
    // than being re-allocated per event.
    private val rotationMatrix = FloatArray(9)
    private val remappedMatrix = FloatArray(9)
    private val orientation = FloatArray(3)

    /**
     * Register for rotation vector sensor updates. Called once per android_main
     * — which is once per Activity *instance*, not once per process, so this
     * must be idempotent: see the guards below.
     */
    @JvmStatic
    @Synchronized
    fun register(activity: Activity) {
        // A second Activity means a second android_main and a second call here.
        // If the previous listener is still registered — the new Activity can
        // resume before this runs, and that onActivityResumed re-registers
        // whatever `listener` holds — unregister it now, before the overwrites
        // below orphan it into exactly the everlasting sensor drain this class
        // comment says was fixed.
        stopListening()

        val manager = activity.getSystemService(Context.SENSOR_SERVICE) as SensorManager?
            ?: return
        sensorManager = manager

        val sensor = manager.getDefaultSensor(Sensor.TYPE_ROTATION_VECTOR) ?: return
        rotationSensor = sensor

        // Resolve the display whose rotation the sensor callback's remap needs,
        // without retaining the Activity (nothing in this class may). The *id*
        // comes from the Activity, the handle from DisplayManager on the
        // application context: Activity.getDisplay() only exists from API 30,
        // and WindowManager.getDefaultDisplay() is only deprecated *at* 30, so
        // each side of the minSdk 28 / targetSdk 34 range uses the call that is
        // correct for it.
        var displayId = Display.DEFAULT_DISPLAY
        if (Build.VERSION.SDK_INT >= 30) {
            activity.display?.let { displayId = it.displayId }
        } else {
            @Suppress("DEPRECATION")
            displayId = activity.windowManager.defaultDisplay.displayId
        }
        val dm = activity.applicationContext
            .getSystemService(Context.DISPLAY_SERVICE) as DisplayManager?
        display = dm?.getDisplay(displayId)

        // An anonymous implementation on a *object* singleton, so it holds no
        // reference to the Activity. Neither does SensorManager, which is a
        // process-scoped system service. Nothing here keeps the Activity alive.
        listener = object : SensorEventListener {
            override fun onSensorChanged(event: SensorEvent) {
                SensorManager.getRotationMatrixFromVector(rotationMatrix, event.values)

                // getOrientation() answers azimuth for the device's *natural*
                // orientation; the UI wants heading relative to what is
                // currently up on screen. Remap the frame by the display
                // rotation first — unremapped, landscape reads ±90° off and
                // reverse portrait 180°. Read per event, not latched in
                // register(): the manifest's configChanges keeps the Activity
                // alive across rotation, so no lifecycle callback marks the
                // change. The axis pairs are the standard remapCoordinateSystem
                // permutations for each Surface.ROTATION_* (which requires
                // out != in, hence the second matrix).
                var m = rotationMatrix
                when (display?.rotation ?: Surface.ROTATION_0) {
                    Surface.ROTATION_90 -> {
                        SensorManager.remapCoordinateSystem(
                            rotationMatrix,
                            SensorManager.AXIS_Y,
                            SensorManager.AXIS_MINUS_X,
                            remappedMatrix,
                        )
                        m = remappedMatrix
                    }
                    Surface.ROTATION_180 -> {
                        SensorManager.remapCoordinateSystem(
                            rotationMatrix,
                            SensorManager.AXIS_MINUS_X,
                            SensorManager.AXIS_MINUS_Y,
                            remappedMatrix,
                        )
                        m = remappedMatrix
                    }
                    Surface.ROTATION_270 -> {
                        SensorManager.remapCoordinateSystem(
                            rotationMatrix,
                            SensorManager.AXIS_MINUS_Y,
                            SensorManager.AXIS_X,
                            remappedMatrix,
                        )
                        m = remappedMatrix
                    }
                    // ROTATION_0: already the natural frame.
                    else -> {}
                }

                SensorManager.getOrientation(m, orientation)
                // orientation[0] is azimuth in radians (-π to π) → degrees, 0–360.
                var azimuth = Math.toDegrees(orientation[0].toDouble()).toFloat()
                if (azimuth < 0f) azimuth += 360f
                headingDeg = azimuth
            }

            override fun onAccuracyChanged(sensor: Sensor?, accuracy: Int) {
                // ignored
            }
        }

        startListening()

        // register() is called from android_main, which runs after onCreate and
        // therefore after the onActivityResumed for this Activity has already
        // gone by. Hence the startListening() above: the callbacks below take
        // over from the first pause onwards.
        //
        // The app declares a single Activity (android.app.NativeActivity, in a
        // singleTask task), so there is no need to match on identity here — and
        // not holding an Activity reference is what keeps this leak-free. It is
        // also what lets one registration serve every later Activity instance,
        // which is why `callbacksRegistered` gates it rather than this stacking
        // a fresh callback per register() call.
        val app = activity.application
        if (app != null && !callbacksRegistered) {
            callbacksRegistered = true
            app.registerActivityLifecycleCallbacks(object : Application.ActivityLifecycleCallbacks {
                override fun onActivityResumed(activity: Activity) = startListening()
                override fun onActivityPaused(activity: Activity) = stopListening()
                override fun onActivityCreated(activity: Activity, savedInstanceState: Bundle?) {}
                override fun onActivityStarted(activity: Activity) {}
                override fun onActivityStopped(activity: Activity) {}
                override fun onActivitySaveInstanceState(activity: Activity, outState: Bundle) {}
                override fun onActivityDestroyed(activity: Activity) {}
            })
        }
    }

    @Synchronized
    private fun startListening() {
        if (listening) return
        val manager = sensorManager ?: return
        val l = listener ?: return
        val sensor = rotationSensor ?: return
        manager.registerListener(l, sensor, SensorManager.SENSOR_DELAY_UI)
        listening = true
    }

    @Synchronized
    private fun stopListening() {
        if (!listening) return
        val manager = sensorManager
        val l = listener
        if (manager != null && l != null) manager.unregisterListener(l)
        listening = false
        // Drop the stale reading rather than leaving the last one to be served
        // as current for however long the app stays backgrounded.
        headingDeg = -1f
    }

    /**
     * The current compass heading in degrees (0–360), or -1 when there is no
     * reading yet or the app is paused. Called over JNI from
     * `get_compass_heading()`; JNI signature `()F`.
     *
     * `@JvmStatic` is load-bearing: without it this is an instance method and
     * the `CallStaticFloatMethod` on the Rust side throws NoSuchMethodError.
     */
    @JvmStatic
    fun getHeading(): Float = headingDeg
}
