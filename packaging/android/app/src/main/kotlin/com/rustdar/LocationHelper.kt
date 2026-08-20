package com.rustdar

import android.app.Activity
import android.app.Application
import android.content.Context
import android.location.Location
import android.location.LocationListener
import android.location.LocationManager
import android.os.Bundle
import android.os.Handler
import android.os.Looper

/**
 * Static helper that holds a location-update subscription open while the
 * Activity is resumed, and caches the freshest fix it is handed for the Rust
 * side's 10 s poll to read.
 *
 * Loaded by name through the app ClassLoader from
 * `rustdar_location::android::init`; the poll and the start/stop calls live in
 * rustdar-location/src/android/location.rs.
 *
 * ## Why a subscription exists at all
 *
 * `getLastKnownLocation()` is passive: it returns whatever fix a provider last
 * produced for *some* client, and with no client requesting updates the
 * providers produce nothing — the poll reads null forever, permission granted or
 * not. The subscription below makes this app that client.
 *
 * The listener also **caches** what it is handed, and the Rust poll reads the
 * cache first. `getLastKnownLocation()` stays as the cold-start read: the cache
 * is empty until the first delivery, and the passive fix is what the app opens
 * on. All fix extraction (altitude, speed, bearing, provider → quality) stays in
 * one place on the Rust side either way — this class hands back the `Location`
 * object itself, not decomposed fields, so there is exactly one decoder.
 *
 * The poll is deliberately still a poll rather than a push: a push would need
 * JNI plumbing in the other direction for a reader that already exists, and it
 * would deliver on the UI thread. See start_location_updates() in
 * rustdar-location for why this shape was chosen over the alternatives.
 *
 * ## Startup is two-phase
 *
 * [register] runs once per android_main, before any permission has been granted;
 * it only stashes what [start] will need. start() arrives over JNI from the Rust
 * permission gate once it observes the runtime permission as granted — minutes
 * later, or never. Only after both does a subscription have any reason to exist.
 * [stop] is the other end of the same wire: the user turning location off, or a
 * grant withdrawn in system settings.
 *
 * ## Lifecycle
 *
 * Mirrors [CompassHelper]: updates run only while the Activity is resumed,
 * driven by ActivityLifecycleCallbacks registered once per process, so a
 * minimised app is not holding GPS on. (Background delivery would be throttled
 * to near-uselessness anyway without ACCESS_BACKGROUND_LOCATION, which this app
 * has no reason to request.)
 */
object LocationHelper {

    /**
     * Update interval asked of the providers. The Rust poll reads every 10 s, so
     * half that keeps the cached fix at most one poll interval stale without
     * asking the hardware for a cadence nothing consumes.
     */
    private const val MIN_TIME_MS = 5000L

    // Written by register() on the android_main thread or flipped by
    // start()/stop() on whichever thread the permission gate runs on; read on
    // the UI thread. Volatile for the same happens-before reasons documented at
    // length in CompassHelper. The subscribe/unsubscribe transitions themselves
    // (`listener`, `listening`) are confined to the main thread: the lifecycle
    // callbacks run there, and start()/stop() post there rather than touching
    // them directly.
    @Volatile
    private var locationManager: LocationManager? = null

    @Volatile
    private var mainHandler: Handler? = null

    /** start() has been called: a permission is granted and updates are wanted. */
    @Volatile
    private var started: Boolean = false

    /**
     * Between onResume and onPause. register() initialises it true — it runs
     * after this Activity's onActivityResumed has already gone by, exactly as
     * described in CompassHelper.register().
     */
    @Volatile
    private var resumed: Boolean = false

    /** One process-wide callback registration; register() runs once per Activity. */
    @Volatile
    private var callbacksRegistered: Boolean = false

    /**
     * The freshest delivered fix and the provider that produced it. Written on
     * the UI thread by the listener, read over JNI on the gps-location thread.
     */
    @Volatile
    private var cachedLocation: Location? = null

    @Volatile
    private var cachedProvider: String? = null

    // Main thread only from here down.
    private var listener: LocationListener? = null
    private var listening: Boolean = false

    /** Stash what start() will need. Called once per android_main over JNI. */
    @JvmStatic
    fun register(activity: Activity) {
        locationManager = activity.getSystemService(Context.LOCATION_SERVICE) as LocationManager?
        mainHandler = Handler(Looper.getMainLooper())
        resumed = true

        // Second Activity: the fields above are refreshed (same process-wide
        // services either way) and the callbacks below are already in place.
        if (callbacksRegistered) return
        val app = activity.application ?: return
        callbacksRegistered = true

        // Same shape as CompassHelper: nothing here retains the Activity, and
        // the app declares a single Activity so no identity matching is needed.
        app.registerActivityLifecycleCallbacks(object : Application.ActivityLifecycleCallbacks {
            override fun onActivityResumed(activity: Activity) {
                resumed = true
                startListening()
            }

            override fun onActivityPaused(activity: Activity) {
                resumed = false
                stopListening()
            }

            override fun onActivityCreated(activity: Activity, savedInstanceState: Bundle?) {}
            override fun onActivityStarted(activity: Activity) {}
            override fun onActivityStopped(activity: Activity) {}
            override fun onActivitySaveInstanceState(activity: Activity, outState: Bundle) {}
            override fun onActivityDestroyed(activity: Activity) {}
        })
    }

    /**
     * Begin location updates. Called over JNI from the Rust permission gate once
     * it observes the runtime permission as granted; JNI signature `()V`.
     * Idempotent, and safe to call in any lifecycle state: the actual subscribe
     * runs on the main thread and re-checks.
     */
    @JvmStatic
    fun start() {
        started = true
        mainHandler?.post { startListening() }
    }

    /**
     * Drop the subscription. Called over JNI when the user turns location off in
     * settings, or when the app observes the runtime permission revoked; JNI
     * signature `()V`. Idempotent, and safe in any lifecycle state for the same
     * reason start() is.
     *
     * Clearing `started` matters as much as the unsubscribe: without it the next
     * onActivityResumed would call startListening() and quietly turn the
     * providers back on for a user who had switched them off.
     */
    @JvmStatic
    fun stop() {
        started = false
        mainHandler?.post { stopListening() }
    }

    /**
     * The freshest fix the subscription has been handed, or null before the
     * first delivery. JNI signature `()Landroid/location/Location;`; the Rust
     * poll falls back to getLastKnownLocation() on null.
     */
    @JvmStatic
    fun cachedFix(): Location? = cachedLocation

    /**
     * The provider name that produced [cachedFix], or null. JNI signature
     * `()Ljava/lang/String;`. Read separately rather than off the Location so a
     * provider-less fix still maps to a quality on the Rust side.
     */
    @JvmStatic
    fun cachedFixProvider(): String? = cachedProvider

    // Main thread only (lifecycle callbacks, or posted from start()).
    private fun startListening() {
        if (listening || !started || !resumed) return
        val lm = locationManager ?: return

        val l = listener ?: object : LocationListener {
            override fun onLocationChanged(location: Location) {
                cachedProvider = location.provider
                cachedLocation = location
            }

            // Explicit no-op overrides, not reliance on the interface's default
            // methods: those defaults only exist from the API 30 interface
            // onward, and on the API 28/29 devices minSdk admits a missing
            // override is an AbstractMethodError the moment a provider toggles.
            // (onStatusChanged is deprecated against compileSdk, but it is these
            // older devices that still call it.)
            @Deprecated("Deprecated in Java")
            override fun onStatusChanged(provider: String?, status: Int, extras: Bundle?) {}
            override fun onProviderEnabled(provider: String) {}
            override fun onProviderDisabled(provider: String) {}
        }.also { listener = it }

        // gps needs ACCESS_FINE_LOCATION; network serves under COARSE alone.
        // Requesting per provider and catching per provider is what makes an
        // "approximate only" grant work instead of aborting on the first
        // SecurityException. The (String, long, float, LocationListener, Looper)
        // overload is present and undeprecated across the whole minSdk 28 ..
        // targetSdk 34 range — the Criteria overloads are the deprecated ones,
        // and getCurrentLocation does not exist before 30.
        var any = false
        for (provider in arrayOf(LocationManager.GPS_PROVIDER, LocationManager.NETWORK_PROVIDER)) {
            try {
                lm.requestLocationUpdates(provider, MIN_TIME_MS, 0f, l, Looper.getMainLooper())
                any = true
            } catch (e: SecurityException) {
                // COARSE-only grant hitting the gps provider; keep going.
            } catch (e: IllegalArgumentException) {
                // Provider does not exist on this device; keep going.
            }
        }
        listening = any
    }

    // Main thread only.
    private fun stopListening() {
        if (!listening) return
        val lm = locationManager
        val l = listener
        if (lm != null && l != null) lm.removeUpdates(l)
        listening = false
    }
}
