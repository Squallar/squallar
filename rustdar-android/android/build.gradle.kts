// Top-level build file. Plugin versions live here so every module shares them.
//
// AGP must be 9.x: Gradle 9.6.0 removed the internal `InternalProblems` API that
// AGP 8.x binds to, so an 8.x plugin fails at apply-time against the wrapper
// version pinned in gradle/wrapper/gradle-wrapper.properties.
plugins {
    id("com.android.application") version "9.3.1" apply false
}
