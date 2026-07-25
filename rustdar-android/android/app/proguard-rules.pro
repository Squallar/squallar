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
    public static void unregister();
    public static float getHeading();
}

# ---------------------------------------------------------------------------
# NativeActivity
# ---------------------------------------------------------------------------
# Named as a string in AndroidManifest.xml and instantiated reflectively by the
# framework.
-keep class android.app.NativeActivity { *; }

# Keep the annotation that guards CompassHelper's API-33-only branch so R8's
# optimizer does not mistake the guarded call for unconditionally reachable.
-keepattributes *Annotation*
