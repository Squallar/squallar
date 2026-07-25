pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

// FAIL_ON_PROJECT_REPOS is deliberately NOT used here. The rustls-platform-verifier
// Kotlin component ships as an AAR *inside the cargo registry checkout* of the
// `rustls-platform-verifier-android` crate, so `:app` has to declare a `maven {}`
// repository whose URL is computed at configuration time by shelling out to
// `cargo metadata`. That is a project-local repository by definition and cannot be
// hoisted here, because the path depends on which crate version Cargo resolved.
// PREFER_PROJECT keeps the shared repos below as the default for every other module
// while still allowing :app its own.
dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.PREFER_PROJECT)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "rustdar"
include(":app")
