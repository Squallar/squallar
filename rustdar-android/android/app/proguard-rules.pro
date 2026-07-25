# R8 rules for the release build.
#
# Almost all of this app is native code in librustdar_android.so, which R8 neither
# sees nor touches. What is left in the DEX is reached exclusively by *name* from
# JNI, so R8's reachability analysis sees no callers and would strip or rename all
# of it. Every rule below exists because of that; none is precautionary.

# ---------------------------------------------------------------------------
# rustls-platform-verifier
# ---------------------------------------------------------------------------
# The Rust verifier calls org.rustls.platformverifier.CertificateVerifier by name
# and reads back StatusCode / VerificationResult. Renaming any of it breaks TLS
# entirely, at runtime, with an error that looks like a certificate problem rather
# than a build problem.
-keep class org.rustls.platformverifier.** { *; }

# ---------------------------------------------------------------------------
# JNI helper classes
# ---------------------------------------------------------------------------
# Loaded via ClassLoader.loadClass("com.rustdar.…") from android_main, then
# invoked through call_static_method by literal method name and signature.
-keep class com.rustdar.BackHandler {
    public static void register(android.app.Activity);
}
-keep class com.rustdar.CompassHelper {
    public static void register(android.app.Activity);
    public static float getHeading();
}
# `unregister` is deliberately absent from that list. It used to be kept here
# and called from nowhere -- the rotation-vector listener stayed registered at
# SENSOR_DELAY_UI for the life of the process, including after the app was
# minimised. CompassHelper now drives it from ActivityLifecycleCallbacks, so
# there is a real caller inside the class and R8 keeps it on its own.

# ---------------------------------------------------------------------------
# Rules that used to be here, and why they are not
# ---------------------------------------------------------------------------
# `-keep class android.app.NativeActivity { *; }` kept nothing. NativeActivity
# is a framework class on the boot classpath and is not in this app's DEX, so
# there is no class member for R8 to retain. What actually matters is the
# no-arg constructor the framework instantiates by name from the manifest, and
# aapt2 already emits `-keep class android.app.NativeActivity { <init>(); }`
# into the generated aapt_rules.txt from the <activity> element itself.
#
# `-keepattributes *Annotation*` was justified in a comment as protecting
# "CompassHelper's API-33-only branch". That branch is not in CompassHelper --
# @TargetApi(33) is on BackHandler.registerBackCallback -- and @TargetApi is
# CLASS-retention, so it does not survive to runtime and takes no part in R8's
# reachability analysis either way. The attributes anything here does need
# (RuntimeVisibleAnnotations, Signature, InnerClasses, ...) are already kept by
# proguard-android-optimize.txt, which this build lists first.
