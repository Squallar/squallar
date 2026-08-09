package com.rustdar;

import android.app.Activity;
import android.app.Application;
import android.content.Context;
import android.location.Location;
import android.location.LocationListener;
import android.location.LocationManager;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;

/**
 * Static helper that holds a location-update subscription open while the
 * Activity is resumed, so the Rust side's getLastKnownLocation() poll has
 * something to read.
 *
 * Loaded by name through the app ClassLoader from android_main; see
 * register_java_helper in rustdar-android/src/lib.rs.
 *
 * <h2>Why a subscription exists at all</h2>
 *
 * getLastKnownLocation() is passive: it returns whatever fix a provider last
 * produced for <em>some</em> client, and with no client requesting updates the
 * providers produce nothing — the poll reads null forever, permission granted
 * or not. The subscription below makes this app that client. The listener
 * itself deliberately records nothing: the running providers refresh the
 * last-known fix as a side effect, the Rust gps-location thread keeps polling
 * it every 10 s, and all fix extraction (altitude, speed, bearing, provider →
 * quality) stays in one place, in lib.rs. A listener that forwarded fixes
 * would need JNI plumbing in the other direction for a poll that already
 * exists; see start_location_updates() in lib.rs for why this shape was chosen
 * over the alternatives.
 *
 * <h2>Startup is two-phase</h2>
 *
 * {@link #register} runs once per android_main, before any permission has been
 * granted; it only stashes what {@link #start} will need. start() arrives over
 * JNI from the Rust permission gate once it observes the runtime permission as
 * granted — minutes later, or never. Only after both does a subscription have
 * any reason to exist. {@link #stop} is the other end of the same wire: the
 * user turning location off, or a grant withdrawn in system settings.
 *
 * <h2>Lifecycle</h2>
 *
 * Mirrors CompassHelper: updates run only while the Activity is resumed,
 * driven by ActivityLifecycleCallbacks registered once per process, so a
 * minimised app is not holding GPS on. (Background delivery would be throttled
 * to near-uselessness anyway without ACCESS_BACKGROUND_LOCATION, which this
 * app has no reason to request.)
 */
public final class LocationHelper {
    // Written by register() on the android_main thread or flipped by
    // start()/stop() on whichever thread the permission gate runs on; read on
    // the UI thread. Volatile for the same happens-before reasons documented at
    // length in CompassHelper. The subscribe/unsubscribe transitions themselves
    // (sListener, sListening) are confined to the main thread: the lifecycle
    // callbacks run there, and start()/stop() post there rather than touching
    // them directly.
    private static volatile LocationManager sLocationManager;
    private static volatile Handler sMainHandler;
    /** start() has been called: a permission is granted and updates are wanted. */
    private static volatile boolean sStarted;
    /**
     * Between onResume and onPause. register() initialises it true — it runs
     * after this Activity's onActivityResumed has already gone by, exactly as
     * described in CompassHelper.register().
     */
    private static volatile boolean sResumed;
    /** One process-wide callback registration; register() itself runs once per Activity. */
    private static volatile boolean sCallbacksRegistered;

    // Main thread only from here down.
    private static LocationListener sListener;
    private static boolean sListening;

    /**
     * Update interval asked of the providers. The Rust poll reads every 10 s,
     * so half that keeps the last-known fix at most one poll interval stale
     * without asking the hardware for a cadence nothing consumes.
     */
    private static final long MIN_TIME_MS = 5000;

    /** Stash what start() will need. Called once per android_main over JNI. */
    public static void register(Activity activity) {
        sLocationManager = (LocationManager) activity.getSystemService(Context.LOCATION_SERVICE);
        sMainHandler = new Handler(Looper.getMainLooper());
        sResumed = true;

        // Second Activity: the fields above are refreshed (same process-wide
        // services either way) and the callbacks below are already in place.
        if (sCallbacksRegistered) return;
        Application app = activity.getApplication();
        if (app == null) return;
        sCallbacksRegistered = true;

        // Same shape as CompassHelper: nothing here retains the Activity, and
        // the app declares a single Activity so no identity matching is needed.
        app.registerActivityLifecycleCallbacks(new Application.ActivityLifecycleCallbacks() {
            @Override
            public void onActivityResumed(Activity a) {
                sResumed = true;
                startListening();
            }

            @Override
            public void onActivityPaused(Activity a) {
                sResumed = false;
                stopListening();
            }

            @Override public void onActivityCreated(Activity a, Bundle b) { }
            @Override public void onActivityStarted(Activity a) { }
            @Override public void onActivityStopped(Activity a) { }
            @Override public void onActivitySaveInstanceState(Activity a, Bundle b) { }
            @Override public void onActivityDestroyed(Activity a) { }
        });
    }

    /**
     * Begin location updates. Called over JNI from the Rust permission gate
     * once it observes the runtime permission as granted; kept by name in
     * proguard-rules.pro. Idempotent, and safe to call in any lifecycle state:
     * the actual subscribe runs on the main thread and re-checks.
     */
    public static void start() {
        sStarted = true;
        Handler h = sMainHandler;
        if (h != null) h.post(LocationHelper::startListening);
    }

    /**
     * Drop the subscription. Called over JNI when the user turns location off
     * in settings, or when the app observes the runtime permission revoked;
     * kept by name in proguard-rules.pro. Idempotent, and safe in any lifecycle
     * state for the same reason start() is.
     *
     * Clearing sStarted matters as much as the unsubscribe: without it the next
     * onActivityResumed would call startListening() and quietly turn the
     * providers back on for a user who had switched them off.
     */
    public static void stop() {
        sStarted = false;
        Handler h = sMainHandler;
        if (h != null) h.post(LocationHelper::stopListening);
    }

    // Main thread only (lifecycle callbacks, or posted from start()).
    private static void startListening() {
        if (sListening || !sStarted || !sResumed) return;
        LocationManager lm = sLocationManager;
        if (lm == null) return;

        if (sListener == null) {
            sListener = new LocationListener() {
                // Deliberately empty — see the class comment. The subscription
                // existing is the point; the fixes are read back through
                // getLastKnownLocation() on the Rust side.
                @Override public void onLocationChanged(Location location) { }

                // Explicit no-op overrides, not reliance on the interface's
                // default methods: those defaults only exist from the API 30
                // interface onward, and on the API 28/29 devices minSdk admits
                // a missing override is an AbstractMethodError the moment a
                // provider toggles. (onStatusChanged is deprecated against
                // compileSdk, but it is these older devices that still call it.)
                @Override public void onStatusChanged(String provider, int status, Bundle extras) { }
                @Override public void onProviderEnabled(String provider) { }
                @Override public void onProviderDisabled(String provider) { }
            };
        }

        // gps needs ACCESS_FINE_LOCATION; network serves under COARSE alone.
        // Requesting per provider and catching per provider is what makes an
        // "approximate only" grant work instead of aborting on the first
        // SecurityException. The (String, long, float, LocationListener,
        // Looper) overload is present and undeprecated across the whole
        // minSdk 28..targetSdk 34 range — the Criteria overloads are the
        // deprecated ones, and getCurrentLocation does not exist before 30.
        boolean any = false;
        for (String provider : new String[] {
                LocationManager.GPS_PROVIDER, LocationManager.NETWORK_PROVIDER }) {
            try {
                lm.requestLocationUpdates(provider, MIN_TIME_MS, 0f, sListener,
                        Looper.getMainLooper());
                any = true;
            } catch (SecurityException e) {
                // COARSE-only grant hitting the gps provider; keep going.
            } catch (IllegalArgumentException e) {
                // Provider does not exist on this device; keep going.
            }
        }
        sListening = any;
    }

    // Main thread only.
    private static void stopListening() {
        if (!sListening) return;
        LocationManager lm = sLocationManager;
        if (lm != null && sListener != null) lm.removeUpdates(sListener);
        sListening = false;
    }
}
