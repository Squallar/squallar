package app.squallar

import android.app.Activity
import android.os.Build
import android.util.Log
import android.window.OnBackInvokedCallback
import android.window.OnBackInvokedDispatcher

/**
 * Back-navigation callback for NativeActivity.
 *
 * ## This class is wired, correct, and NOT CURRENTLY INVOKED
 *
 * Back reaches this app one way today: as KEYCODE_BACK through the native input
 * queue, handled in Rust — InputHandler::take_back_out_press, then
 * App::back_out, then App::resolve_back_press. That is the route on every API
 * level this app runs on, 33+ included.
 *
 * The other route, `OnBackInvokedDispatcher`, is reached only by an app that
 * sets `android:enableOnBackInvokedCallback="true"`, and this app's manifest
 * deliberately does not — because it was tried and measured: with the opt-in,
 * an unclaimed press backgrounds the app and the app can then never be reopened
 * (a relaunch starts a fresh NativeActivity, and `android_main` does not run for
 * a second Activity in this process). The manifest carries the full reading.
 * **So the callback this class registers is inert, exactly as the Java class it
 * replaced was.**
 *
 * It is wired anyway, and truthfully, for one reason: from targetSdk 36 the
 * opt-in is not optional. When the `android_main`-per-Activity fix lands, the
 * only remaining change is the manifest attribute — no design work, and no
 * class that has to be invented under a deadline.
 *
 * ## Why this class registers and unregisters rather than always consuming
 *
 * `OnBackInvokedCallback.onBackInvoked()` returns void, so "the app declined
 * this press" cannot be expressed after the fact. The only truthful way to
 * decline is to not be registered when the press arrives — and that is exactly
 * what buys the system's own back-to-home preview animation, which an app that
 * always consumes can never show.
 *
 * So registration is a *claim*, pushed from Rust through [setClaimed] whenever
 * `Gui::back_would_dismiss()` changes: registered means "there is an open sheet,
 * drawer, dialog or armed drag for this press to close", unregistered means
 * "nothing is open, let the platform do its thing". Keeping that claim truthful
 * at every sheet transition is the whole obligation this class places on the
 * Rust side; nothing here decides anything.
 *
 * There is deliberately **no moveTaskToBack in this class**. Minimising is a
 * decision, and this class makes none: under the opt-in an unregistered app is
 * backgrounded by the platform itself, and a registered one has something for
 * Rust to close. A claim that has gone stale still lands safely: the press
 * routes into App::resolve_back_press, which minimises a press that dismisses
 * nothing through its own PlatformBridge::handle_back — which is also the
 * minimise the legacy route uses today.
 *
 * The base [OnBackInvokedCallback], deliberately, and not the animation-aware
 * subinterface: this app draws no in-app back animation, so it wants the commit
 * edge and nothing else. The source pin in squallar-app/src/app/tests.rs greps
 * this file for the subinterface's name and is why it is not spelled here.
 *
 * Loaded by name through the app ClassLoader from android_main; see
 * register_java_helper in squallar/src/android/mod.rs.
 */
object BackHandler {

    private const val TAG = "squallar"

    init {
        // MEASURED, not defensive (WO-RP-SPIKE, 2026-08-20): without this the
        // callback below reaches `nativeBackPressed()` and ART answers
        //
        //   No implementation found for void app.squallar.BackHandler.nativeBackPressed()
        //   (tried Java_app_squallar_BackHandler_nativeBackPressed ...)
        //   - is the library loaded, e.g. System.loadLibrary?
        //
        // even though the symbol is exported from libsquallar_native.so and the
        // process has had it mapped since before onCreate returned.
        //
        // The reason is how NativeActivity loads it. NativeActivity.onCreate
        // resolves android.app.lib_name to a path and hands that path to its own
        // `loadNativeCode` native method, which calls dlopen() directly. ART is
        // never told, so the library is absent from the JavaVM's library list —
        // and that list, keyed by ClassLoader, is the only thing
        // ArtMethod::FindNativeMethod searches when binding a native declared on
        // an app class. A raw dlopen serves android_main perfectly well (the
        // framework dlsym()s that symbol itself) and serves an app class not at
        // all.
        //
        // System.loadLibrary here is what puts the *already mapped* library on
        // that list under this class's own loader: dlopen returns the existing
        // handle, nothing is mapped twice, and there is no JNI_OnLoad in a Rust
        // cdylib to re-run. The name is squallar's [lib] name, which is also the
        // manifest's android.app.lib_name — three spellings of one library,
        // pinned together in `the_kotlin_helper_loads_the_library_the_manifest_names`.
        //
        // The Java class this replaced had the same defect and a doc comment
        // asserting the opposite ("which NativeActivity has already loaded
        // through this same ClassLoader"). It went unnoticed because the app was
        // never opted into the predictive-back dispatcher, so that callback
        // never fired. The throwaway opt-in build WO-RP-SPIKE used to exercise
        // this route is what made the path live and exposed it; the app ships
        // opted OUT (see the manifest), so this stays untriggered here too —
        // and stays, because it is the difference between a working route and a
        // dropped press the day the opt-in becomes mandatory.
        System.loadLibrary("squallar_native")
    }

    /**
     * The Activity whose dispatcher [setClaimed] registers against. Written on
     * the android_main thread by [register], read on the UI thread and on
     * whichever thread pushes a claim, hence `@Volatile`.
     */
    @Volatile
    private var activity: Activity? = null

    /**
     * The callback instance and whether it is currently registered. Main thread
     * only — every mutation runs inside the [applyClaim] posted below.
     *
     * `Any?` rather than `OnBackInvokedCallback?`: minSdk is 28 and the field
     * type would put an API 33 class in this object's `<clinit>`, which fails
     * to resolve on every device below 33 — taking [register] down with it.
     * Creation is confined to [applyClaim], which never runs below 33.
     */
    private var callback: Any? = null
    private var registered = false

    /**
     * Stash the Activity. Called once per android_main over JNI with signature
     * `(Landroid/app/Activity;)V` — the same one register_java_helper invokes
     * on every helper.
     *
     * Registering nothing here is the point: at this moment the app has no UI
     * open, so it has no claim to make.
     */
    @JvmStatic
    fun register(activity: Activity) {
        this.activity = activity
    }

    /**
     * Publish Rust's claim on the next back press: `true` registers the
     * callback, `false` unregisters it. Idempotent, and a no-op below API 33
     * where the legacy KEYCODE_BACK route is the delivery instead.
     *
     * Called over JNI from the Rust frame loop, edge-triggered — only when the
     * claim actually changes — because a per-frame JNI hop is a cost at 120 Hz
     * that buys nothing.
     */
    @JvmStatic
    fun setClaimed(claimed: Boolean) {
        if (Build.VERSION.SDK_INT < 33) return
        val a = activity ?: return
        a.runOnUiThread { applyClaim(a, claimed) }
    }

    /** Main thread only. Never reached below API 33 — see [callback]. */
    private fun applyClaim(a: Activity, claimed: Boolean) {
        if (claimed == registered) return
        val dispatcher = a.onBackInvokedDispatcher
        if (claimed) {
            val cb = (callback as OnBackInvokedCallback?)
                ?: OnBackInvokedCallback { onBackInvoked() }.also { callback = it }
            dispatcher.registerOnBackInvokedCallback(
                OnBackInvokedDispatcher.PRIORITY_DEFAULT,
                cb,
            )
        } else {
            (callback as OnBackInvokedCallback?)?.let {
                dispatcher.unregisterOnBackInvokedCallback(it)
            }
        }
        registered = claimed
    }

    /**
     * Hand one press to Rust. Throwable, not Exception: a missing native symbol
     * is an UnsatisfiedLinkError, and swallowing it here beats an uncaught throw
     * on the UI thread from inside a framework dispatch.
     */
    private fun onBackInvoked() {
        try {
            nativeBackPressed()
        } catch (t: Throwable) {
            Log.e(TAG, "nativeBackPressed() is unreachable; the press is dropped", t)
        }
    }

    /**
     * Park a back press for the winit event loop and wake it. Void: this side
     * has no decision left to express. Implemented in libsquallar_native.so as
     * `Java_app_squallar_BackHandler_nativeBackPressed`
     * (squallar/src/android/back.rs).
     *
     * It binds only because of the [System.loadLibrary] in this object's `init`.
     * NativeActivity dlopen()s the library by path and never tells ART, so it is
     * absent from the ClassLoader-keyed list this native method is resolved
     * against — measured, not reasoned; see the init block.
     *
     * `@JvmStatic` is load-bearing: without it this is an *instance* method of
     * the object and the static-call/native-binding contract both ends assume
     * no longer holds.
     */
    @JvmStatic
    external fun nativeBackPressed()
}
