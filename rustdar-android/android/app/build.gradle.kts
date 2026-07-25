import org.gradle.api.artifacts.dsl.RepositoryHandler
import org.gradle.api.artifacts.repositories.MavenArtifactRepository
import org.gradle.api.tasks.Exec
import org.gradle.kotlin.dsl.register
import java.io.File
import java.util.Properties

plugins {
    id("com.android.application")
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------
//
// This file lives at `rustdar-android/android/app/`, so the Cargo workspace root
// is three directories up. `rustCrateManifest` is the manifest of the crate that
// actually depends on `rustls-platform-verifier` — that is what the Maven lookup
// below has to be pointed at, not the workspace root.
val rustWorkspaceRoot = rootProject.layout.projectDirectory.dir("../..")
val rustCrateManifest = rootProject.layout.projectDirectory.file("../Cargo.toml")
val jniLibsDir = layout.projectDirectory.dir("src/main/jniLibs")

// ---------------------------------------------------------------------------
// rustls-platform-verifier Kotlin component
// ---------------------------------------------------------------------------
//
// The Android half of `rustls-platform-verifier` is a Kotlin class
// (org.rustls.platformverifier.CertificateVerifier) that the Rust side calls
// through JNI. It is not on Maven Central: it ships as an AAR *inside the cargo
// registry checkout* of the `rustls-platform-verifier-android` crate. So the
// repository URL is only knowable by asking Cargo where it unpacked that crate,
// which is what the `cargo metadata` call below does.
//
// `--filter-platform aarch64-linux-android` matters: `rustls-platform-verifier`
// only depends on `-android` for Android targets, so on a host-platform query the
// package is simply absent from the graph.
//
// This is the snippet from the crate's README, translated to the Kotlin DSL and
// using Gradle's bundled Groovy JsonSlurper rather than kotlinx-serialization so
// that no buildscript classpath dependency is needed.
fun RepositoryHandler.rustlsPlatformVerifier(): MavenArtifactRepository {
    val metadataJson = providers.exec {
        commandLine(
            "cargo", "metadata",
            "--format-version", "1",
            "--filter-platform", "aarch64-linux-android",
            "--manifest-path", rustCrateManifest.asFile.absolutePath,
        )
    }.standardOutput.asText.get()

    @Suppress("UNCHECKED_CAST")
    val packages = (groovy.json.JsonSlurper().parseText(metadataJson) as Map<String, Any>)
        .getValue("packages") as List<Map<String, Any>>

    val verifierPkg = packages.firstOrNull { it["name"] == "rustls-platform-verifier-android" }
        ?: error(
            "rustls-platform-verifier-android is not in the Cargo dependency graph for " +
                "aarch64-linux-android. The Android TLS stack cannot be built without it."
        )

    val manifestPath = File(verifierPkg.getValue("manifest_path") as String)
    val mavenDir = File(manifestPath.parentFile, "maven")
    require(mavenDir.isDirectory) { "Expected bundled Maven repo at $mavenDir" }

    return maven {
        url = uri(mavenDir)
        // The bundled repo has no Gradle module metadata or POM-based variant info;
        // resolve straight from the AAR artifact.
        metadataSources { artifact() }
    }
}

repositories {
    rustlsPlatformVerifier()
}

// ---------------------------------------------------------------------------
// Release signing
// ---------------------------------------------------------------------------
//
// Reads `android/keystore.properties` (or `android/app/keystore.properties`) so a
// release build produces a signed artifact without passing
// `-Pandroid.injected.signing.*` on the command line. Expected keys:
//
//   storeFile=path/to/release.jks      (relative paths resolve against android/)
//   storePassword=...
//   keyAlias=...
//   keyPassword=...
//
// Gitignored. If absent, release builds fall back to unsigned with a warning —
// which is what happens on a fresh clone, since the old cargo-apk config's
// keystore (`../rustdar.jks`, password in cleartext in Cargo.toml) was never
// committed and that password must be treated as burned.
val keystoreProps: Properties? = run {
    val found = listOf(rootProject.file("keystore.properties"), file("keystore.properties"))
        .firstOrNull { it.isFile }
    found?.let { f -> Properties().apply { f.inputStream().use { load(it) } } }
}

// ---------------------------------------------------------------------------
// ABIs
// ---------------------------------------------------------------------------
//
// 64-bit only, and this is a correctness constraint rather than a size one. A
// single radar pane holds up to 20 loop frames at 2048x2048 RGBA — roughly 320 MB
// — and two panes can be open at once. That does not fit in a 32-bit process's
// address space, so armeabi-v7a/x86 builds would OOM rather than merely run slow.
//
// Override for a one-off local build with `-PabiFilter=arm64-v8a`.
val allAbis = listOf("arm64-v8a", "x86_64")
val selectedAbis: List<String> = (project.findProperty("abiFilter") as String?)
    ?.split(",")?.map(String::trim)?.filter(String::isNotEmpty)
    ?.also { sel -> require(sel.all(allAbis::contains)) { "abiFilter $sel not a subset of $allAbis" } }
    ?: allAbis

android {
    namespace = "dev.mcswain.rustdar"
    compileSdk = 36
    ndkVersion = "27.3.13750724"

    defaultConfig {
        applicationId = "dev.mcswain.rustdar"
        minSdk = 28
        // Deliberately held at 34, the value the cargo-apk build shipped.
        //
        // targetSdk 35+ makes edge-to-edge display mandatory and non-opt-out,
        // which changes what getRootWindowInsets() reports. The Rust side feeds
        // those insets straight into the egui layout and into the map's
        // excluded_rects hit-test regions, so raising this silently shifts where
        // clicks land on overlays. Raise it as its own change, with the on-device
        // inset/hit-test checks re-run.
        targetSdk = 34
        versionCode = 1
        versionName = "0.1.0"
    }

    // One APK per ABI rather than a fat APK carrying both native libs; each device
    // downloads only its own slice. `isUniversalApk = false` means no combined APK
    // is emitted alongside. The `include` list is the sole ABI scope — AGP forbids
    // also setting defaultConfig.ndk.abiFilters when splits are configured.
    //
    // The .aab (bundleRelease) is unaffected by this block: App Bundles do ABI
    // splitting themselves at the Play Store, from whatever is in jniLibs — which
    // is exactly these ABIs, because that is all cargo-ndk was asked to build.
    splits {
        abi {
            isEnable = true
            reset()
            include(*selectedAbis.toTypedArray())
            isUniversalApk = false
        }
    }

    signingConfigs {
        if (keystoreProps != null) {
            create("release") {
                val storePath = keystoreProps.getProperty("storeFile")
                    ?: error("keystore.properties: missing storeFile")
                storeFile = file(storePath).let {
                    if (it.isAbsolute) it else rootProject.file(storePath)
                }
                storePassword = keystoreProps.getProperty("storePassword")
                    ?: error("keystore.properties: missing storePassword")
                keyAlias = keystoreProps.getProperty("keyAlias")
                    ?: error("keystore.properties: missing keyAlias")
                keyPassword = keystoreProps.getProperty("keyPassword")
                    ?: error("keystore.properties: missing keyPassword")
            }
        }
    }

    buildTypes {
        getByName("debug") {
            isMinifyEnabled = false
        }
        getByName("release") {
            // Shrinks the DEX, which is almost entirely kotlin-stdlib pulled in by
            // the rustls verifier. Everything reached reflectively or over JNI is
            // kept explicitly in proguard-rules.pro — see the file for why each
            // rule is load-bearing.
            isMinifyEnabled = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
            )
            if (keystoreProps != null) {
                signingConfig = signingConfigs.getByName("release")
            } else {
                logger.warn(
                    "[rustdar] No android/keystore.properties found; release artifacts will be unsigned.\n" +
                        "  Create android/keystore.properties with storeFile/storePassword/keyAlias/keyPassword."
                )
            }
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    // The cdylib is staged into src/main/jniLibs/<abi>/librustdar_android.so by the
    // cargo-ndk tasks below; AGP packages whatever it finds there.
    sourceSets["main"].jniLibs.directories += "src/main/jniLibs"

    packaging {
        jniLibs {
            // Uncompressed and page-aligned .so entries loaded straight from the
            // APK. Required for minSdk 23+ and what the 16 KB-page devices expect.
            useLegacyPackaging = false
        }
    }
}

// ---------------------------------------------------------------------------
// cargo-ndk integration
// ---------------------------------------------------------------------------
//
// Replaces build-android.sh. Two tasks so the cargo profile can differ per build
// type while each stays independently incremental.
//
// NOTE the package is `rustdar-android`, not `rustdar-platform`. `android_main` —
// the symbol NativeActivity dlsym()s after loading the .so named by the
// `android.app.lib_name` manifest meta-data — is defined in rustdar-android.
// rustdar-platform is a plain dependency of it and its own cdylib has no entry
// point, so building that instead yields a library the app cannot start from.
fun cargoNdkTask(name: String, cargoProfile: String): TaskProvider<Exec> = tasks.register<Exec>(name) {
    workingDir = rustWorkspaceRoot.asFile

    // Pin the NDK link platform to minSdk. cargo-ndk otherwise defaults to a much
    // older API level, and the sysroot for that level is missing symbols the
    // graphics/JNI stack links against.
    val ndkPlatform = (android.defaultConfig.minSdk ?: 28).toString()
    val abis = selectedAbis

    val args = mutableListOf("cargo", "ndk")
    abis.forEach { args += listOf("-t", it) }
    args += listOf(
        "-P", ndkPlatform,
        "-o", jniLibsDir.asFile.absolutePath,
        "build",
        "-p", "rustdar-android",
        "--lib",
    )
    if (cargoProfile == "release") args += "--release"
    commandLine = args

    // Full workspace-crate closure of rustdar-android on aarch64-linux-android
    // (regenerate with `cargo metadata --filter-platform aarch64-linux-android`).
    // A crate missing from this list leaves the task UP-TO-DATE after edits to it,
    // and Gradle then packages a stale .so — the failure mode is a build that
    // "succeeds" while shipping last hour's Rust. build.rs goes through files() so
    // crates without one are tolerated, and picked up if one is added later.
    listOf(
        "nexrad-level3",
        "rustdar-android",
        "rustdar-android-theme",
        "rustdar-egui",
        "rustdar-frontend",
        "rustdar-gps",
        "rustdar-overlays",
        "rustdar-platform",
        "rustdar-radar",
        "rustdar-units",
    ).forEach { crate ->
        inputs.dir(rustWorkspaceRoot.dir("$crate/src"))
        inputs.file(rustWorkspaceRoot.file("$crate/Cargo.toml"))
        inputs.files(rustWorkspaceRoot.file("$crate/build.rs"))
    }
    inputs.file(rustWorkspaceRoot.file("Cargo.toml"))
    inputs.file(rustWorkspaceRoot.file("Cargo.lock"))
    inputs.file(rustWorkspaceRoot.file("rust-toolchain.toml"))
    inputs.property("abis", abis)
    inputs.property("profile", cargoProfile)
    abis.forEach { outputs.dir(jniLibsDir.dir(it)) }
}

val buildRustLibDebug = cargoNdkTask("buildRustLibDebug", "debug")
val buildRustLibRelease = cargoNdkTask("buildRustLibRelease", "release")

// No instrumented tests are shipped; disabling the component trims configuration
// time and avoids AGP wiring up an androidTest -> main dependency we never use.
androidComponents.beforeVariants { variantBuilder ->
    (variantBuilder as com.android.build.api.variant.HasAndroidTestBuilder).enableAndroidTest = false
}

androidComponents.onVariants { variant ->
    val cap = variant.name.replaceFirstChar { it.uppercase() }
    val provider = if (variant.buildType == "release") buildRustLibRelease else buildRustLibDebug
    // Covers both the APK path and the .aab path — bundle assembly consumes the
    // same merged native-libs output.
    tasks.matching { it.name == "merge${cap}JniLibFolders" }.configureEach { dependsOn(provider) }
    tasks.matching { it.name == "merge${cap}NativeLibs" }.configureEach { dependsOn(provider) }
}

tasks.named("clean").configure {
    doLast { jniLibsDir.asFile.deleteRecursively() }
}

dependencies {
    // The Kotlin CertificateVerifier the Rust side reaches over JNI. Version is
    // whatever the resolved `rustls-platform-verifier-android` crate bundles, so
    // it tracks the Cargo dependency automatically instead of being pinned twice.
    implementation("rustls:rustls-platform-verifier:latest.release")
}
