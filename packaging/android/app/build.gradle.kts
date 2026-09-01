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
// This file lives at `packaging/android/app/`, so the Cargo workspace root is
// three directories up from here (two up from the Gradle root project at
// `packaging/android/`, which is what the paths below hang off).
// `rustCrateManifest` is the manifest of the crate that actually depends on
// `rustls-platform-verifier` — the `squallar` app crate — because that is what
// the Maven lookup below has to be pointed at, not the workspace root.
val rustWorkspaceRoot = rootProject.layout.projectDirectory.dir("../..")
val rustCrateManifest = rootProject.layout.projectDirectory.file("../../squallar/Cargo.toml")

// ---------------------------------------------------------------------------
// Native-library staging
// ---------------------------------------------------------------------------
//
// One staging directory *per build type*, and that separation is a correctness
// requirement, not tidiness. It used to be a single shared `src/main/jniLibs`,
// and the consequence was that a release APK could silently ship the debug
// library -- no error, no warning, `BUILD SUCCESSFUL`:
//
//   clean && assembleRelease   ->  jniLibs holds the release .so   (~15.9 MB)
//   assembleDebug              ->  jniLibs now holds the debug .so (~23.1 MB)
//   assembleRelease            ->  BUILD SUCCESSFUL, and the APK carries the
//                                  *debug* .so, byte for byte
//
// Gradle does re-run `buildRustLibRelease` there -- the shared directory is
// declared as an output of both tasks, so writing it from one leaves the other
// out of date. The task then exits 0 without doing anything, because cargo-ndk
// only copies its build product into `-o` when the source is *newer* than what
// is already at the destination (`is_fresh`: `src <= dest` means skip). The
// release .so is older than the debug .so that was staged after it, so the copy
// is skipped and `mergeReleaseNativeLibs` packages whatever it finds.
//
// The reverse direction is the same bug and ships an LTO'd, stripped release
// library inside a debuggable APK.
//
// Under `layout.buildDirectory` rather than `src/`, so `clean` removes it as
// part of deleting the build directory. Nothing here is authored.
fun jniLibsDirFor(buildType: String): Directory =
    layout.buildDirectory.dir("jniLibs/$buildType").get()

// Where the shared staging directory used to be. Nothing writes to it any more,
// but it is also AGP's *default* `main` jniLibs source directory, so a copy left
// behind by a pre-fix build would still be packaged. `clean` removes it, and the
// `main` source set is emptied in the `android {}` block below.
val legacyJniLibsDir = layout.projectDirectory.dir("src/main/jniLibs")

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
//
// The version is read from the repo layout rather than declared as
// `latest.release` (which is what the README suggests). Dynamic version selection
// needs a `maven-metadata.xml` to enumerate versions, and the bundled repo ships
// only `maven-metadata-local.xml` — so `latest.release` fails to resolve outright.
// Listing the version directory gets the same "track whatever Cargo resolved"
// behaviour without depending on metadata that is not there.
data class RustlsVerifier(val mavenDir: File, val version: String)

val rustlsVerifier: RustlsVerifier = run {
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

    val version = File(mavenDir, "rustls/rustls-platform-verifier")
        .listFiles { f: File -> f.isDirectory }
        ?.map { it.name }
        ?.maxOrNull()
        ?: error("No versions of rustls:rustls-platform-verifier found under $mavenDir")

    RustlsVerifier(mavenDir, version)
}

repositories {
    // google() and mavenCentral() must be repeated here, not just in settings.
    // Declaring any project-level repository makes Gradle use the project's list
    // instead of the settings list, so omitting them leaves AGP's own
    // dependencies — notably the kotlin-stdlib its built-in Kotlin support adds to
    // the runtime classpath — resolvable only from the rustls repo, where they
    // obviously are not.
    google()
    mavenCentral()
    maven {
        url = uri(rustlsVerifier.mavenDir)
        // mavenPom() is what carries `<packaging>aar</packaging>`; without it
        // Gradle looks for a .jar that does not exist. artifact() is the fallback
        // for a version directory that somehow has no POM.
        metadataSources {
            mavenPom()
            artifact()
        }
    }
}

// ---------------------------------------------------------------------------
// Release signing
// ---------------------------------------------------------------------------
//
// Reads `android/keystore.properties` (or `android/app/keystore.properties`) so a
// release build produces a signed artifact without passing
// `-Pandroid.injected.signing.*` on the command line.
//
// `android/keystore.properties.example` is the committed template: it documents
// every key and carries the `keytool -genkeypair` invocation that produces a
// keystore. Both the real file and `*.jks` are gitignored, because the password
// has to appear in it in the clear.
//
// If absent, release builds still succeed but emit `…-release-unsigned.apk`,
// which Android will not install. That is the failure mode worth naming: the
// obvious workaround is to sideload the *debug* APK instead, and that build is
// `debuggable="true"` under the stock `CN=Android Debug` key. It installs
// alongside a release build rather than over it — see `applicationIdSuffix`
// below — so reaching for it never costs the installed app its data. The warning is
// raised from the release assemble tasks (below) rather than here, so it lands
// at the end of the build it actually applies to instead of on every
// invocation, including `assembleDebug`.
//
// The old cargo-apk keystore (`../squallar.jks`, password in cleartext in
// Cargo.toml) was never committed, but that password is in git history and must
// be treated as burned.
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
// ---------------------------------------------------------------------------
// NDK location
// ---------------------------------------------------------------------------
//
// Resolved explicitly rather than read back off the `android {}` block: AGP 9
// removed `android.ndkDirectory`, and cargo-ndk is a plain subprocess that finds
// the NDK only through the environment. Deriving both from one pinned version
// here is what keeps the Rust and Java halves of the build on the same NDK.
val ndkVersionPin = "27.3.13750724"

val androidSdkDir: File = run {
    val sdkDirProp = rootProject.file("local.properties")
        .takeIf { it.isFile }
        ?.let { f -> Properties().apply { f.inputStream().use { load(it) } }.getProperty("sdk.dir") }
    val path = sdkDirProp
        ?: System.getenv("ANDROID_HOME")
        ?: System.getenv("ANDROID_SDK_ROOT")
        ?: error("Android SDK not found. Set sdk.dir in local.properties, or ANDROID_HOME.")
    File(path)
}

val ndkDir: File = File(androidSdkDir, "ndk/$ndkVersionPin")

val allAbis = listOf("arm64-v8a", "x86_64")
val selectedAbis: List<String> = (project.findProperty("abiFilter") as String?)
    ?.split(",")?.map(String::trim)?.filter(String::isNotEmpty)
    ?.also { sel -> require(sel.all(allAbis::contains)) { "abiFilter $sel not a subset of $allAbis" } }
    ?: allAbis

android {
    namespace = "app.squallar"
    compileSdk = 36
    ndkVersion = ndkVersionPin

    defaultConfig {
        applicationId = "app.squallar"
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
                // Plain `File`, not Project.file(): the latter resolves a
                // relative path against *this* project (app/) and always
                // returns an absolute File, which made the isAbsolute test
                // below a tautology and the rootProject fallback dead code.
                // A relative storeFile must resolve against android/ (the
                // root project) -- that is what keystore.properties.example
                // documents, and where its keytool command writes the
                // keystore.
                storeFile = File(storePath).let {
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
            // A debug build is a *different package* from the release one, so
            // the two coexist on one device.
            //
            // Without this they share `app.squallar`, and because they are
            // signed by different keys — release by the keystore below, debug
            // by the stock `CN=Android Debug` — Android refuses to install
            // either over the other. The only way through is `adb uninstall`,
            // which destroys the installed app's data. On a developer's own
            // handset that is an annoyance; on a phone carrying a real
            // install it is data loss, and it is the *debug* build (the one
            // reached for while diagnosing) that forces the choice.
            //
            // The suffix moves only the applicationId. `namespace` stays
            // `app.squallar`, so the Kotlin helpers keep their Java package
            // and the JNI lookups in `android::entry` — which name
            // `app.squallar.BackHandler` and friends as *class* names —
            // resolve unchanged in both variants.
            applicationIdSuffix = ".debug"
        }
        getByName("release") {
            // Nothing is shrunk, so nothing has to be kept, and that is the trade
            // being made here deliberately.
            //
            // Every class this app owns is reached only reflectively (loaded by
            // name through the app ClassLoader) or over JNI (static calls and
            // native-symbol binding), so R8 sees no reference to any of them.
            // Shrinking therefore required a -keep rule per class and per JNI
            // entry point in a hand-maintained keep file, and the failure mode of
            // a missing one was a NoSuchMethodError on a device in a release
            // build that no host test can reach.
            //
            // The cost of turning it off is DEX size: the ~1.5 MB kotlin-stdlib
            // that arrives with the rustls-platform-verifier AAR now ships
            // unshrunk, alongside this app's own three Kotlin helpers. Against a
            // native library measured in tens of megabytes that is noise.
            // Re-enabling R8 is registered as post-campaign polish, and it has to
            // come back with the keeps, not without them.
            isMinifyEnabled = false
            if (keystoreProps != null) {
                signingConfig = signingConfigs.getByName("release")
            }
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    // The cdylib is staged per build type, and wired to the matching variant in
    // the `androidComponents.onVariants` block below -- never through `main`,
    // which every variant would inherit. Emptying `main` is what makes that
    // stick: `src/main/jniLibs` is AGP's built-in default for this source set,
    // so leaving it populated would let a stale library from a pre-fix build
    // back into both APKs regardless of what the variant sources say.
    sourceSets["main"].jniLibs.directories.clear()

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
// `buildType` names both the cargo profile and the staging directory, and those
// two must not be allowed to drift apart: one directory shared between profiles
// is exactly the bug described at `jniLibsDirFor` above.
//
// NOTE the package is `squallar` — since the android fold there is no separate
// entry crate. `android_main` lives in squallar's cfg(target_os = "android")
// android::entry module, and `android.app.lib_name` names its [lib],
// `squallar_native` -> libsquallar_native.so.
fun cargoNdkTask(name: String, buildType: String): TaskProvider<Exec> = tasks.register<Exec>(name) {
    workingDir = rustWorkspaceRoot.asFile

    // Pin the NDK link platform to minSdk. cargo-ndk otherwise defaults to a much
    // older API level, and the sysroot for that level is missing symbols the
    // graphics/JNI stack links against.
    val ndkPlatform = (android.defaultConfig.minSdk ?: 28).toString()
    val abis = selectedAbis
    val stagingDir = jniLibsDirFor(buildType)

    val args = mutableListOf("cargo", "ndk")
    abis.forEach { args += listOf("-t", it) }
    args += listOf(
        "-P", ndkPlatform,
        "-o", stagingDir.asFile.absolutePath,
        "build",
        "-p", "squallar",
        "--lib",
    )
    if (buildType == "release") args += "--release"
    commandLine = args

    // Point cargo-ndk at the same NDK the `android {}` block pins, rather than
    // letting it pick up whatever ANDROID_NDK_HOME happens to be exported. Without
    // this the Rust and Java halves of the build can silently use different NDKs,
    // and CI -- where no such variable is set -- fails with cargo-ndk's own
    // "couldn't find NDK" rather than anything actionable.
    doFirst {
        require(ndkDir.isDirectory) {
            "NDK $ndkVersionPin not found at $ndkDir. Install it with:\n" +
                "  sdkmanager --install \"ndk;$ndkVersionPin\""
        }
    }
    environment("ANDROID_NDK_HOME", ndkDir.absolutePath)

    // Full in-repo crate closure of `squallar` on aarch64-linux-android
    // (regenerate with `cargo metadata --filter-platform aarch64-linux-android`
    // and list every workspace-path package in squallar's dependency closure --
    // vendor/ members included). A crate missing from this list leaves the task
    // UP-TO-DATE after edits to it, and Gradle then packages a stale .so — the
    // failure mode is a build that "succeeds" while shipping last hour's Rust;
    // the pre-fold list had exactly that bug (squallar-source, squallar-geo and
    // the three vendor crates were missing). build.rs goes through files() so
    // crates without one are tolerated, and picked up if one is added later.
    listOf(
        "nexrad-level3",
        "squallar",
        "squallar-app",
        "squallar-device-profile",
        "squallar-egui",
        "squallar-geo",
        "squallar-gpu",
        "squallar-kv",
        "squallar-location",
        "squallar-nmea-serial",
        "squallar-overlays",
        "squallar-radar",
        "squallar-source",
        "squallar-units",
        "squallar-volumetric",
        "squallar-worker",
        "vendor/bzip2-rs",
        "vendor/nexrad-data",
        "vendor/nexrad-decode",
    ).forEach { crate ->
        inputs.dir(rustWorkspaceRoot.dir("$crate/src"))
        inputs.file(rustWorkspaceRoot.file("$crate/Cargo.toml"))
        inputs.files(rustWorkspaceRoot.file("$crate/build.rs"))
    }
    inputs.file(rustWorkspaceRoot.file("Cargo.toml"))
    inputs.file(rustWorkspaceRoot.file("Cargo.lock"))
    inputs.file(rustWorkspaceRoot.file("rust-toolchain.toml"))
    inputs.property("abis", abis)
    inputs.property("profile", buildType)
    // The whole staging directory, not one entry per ABI: with `-PabiFilter` the
    // set of ABIs varies between invocations, and declaring only the selected
    // ones would leave a library from a previous, wider run in place and
    // undeclared. Gradle removes stale output files it owns.
    outputs.dir(stagingDir)
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
    val buildType = variant.buildType ?: "debug"
    val provider = if (buildType == "release") buildRustLibRelease else buildRustLibDebug

    // Give *this* variant the staging directory its own cargo profile writes to.
    // Per-variant rather than through `sourceSets["main"]`: a `main` entry is
    // inherited by every variant, which is how one directory ended up being both
    // the debug and the release native-library source.
    //
    // `error()` rather than `?.`: a silently skipped wiring here produces an APK
    // with no native library in it at all, which installs and then dies in
    // dlopen at launch. Fail at configuration time instead.
    val jniLibsSources = variant.sources.jniLibs
        ?: error("Variant ${variant.name}: no jniLibs source set to attach the cargo-ndk output to.")
    jniLibsSources.addStaticSourceDirectory(jniLibsDirFor(buildType).asFile.absolutePath)

    // Covers both the APK path and the .aab path — bundle assembly consumes the
    // same merged native-libs output.
    tasks.matching { it.name == "merge${cap}JniLibFolders" }.configureEach { dependsOn(provider) }
    tasks.matching { it.name == "merge${cap}NativeLibs" }.configureEach { dependsOn(provider) }

    // Say so, loudly and at the end, when a release artifact came out unsigned.
    // See the `keystoreProps` block for why that matters more than it looks.
    if (buildType == "release" && keystoreProps == null) {
        tasks.matching { it.name == "assemble$cap" || it.name == "bundle$cap" }.configureEach {
            doLast {
                logger.warn(
                    "\n[squallar] $name: NO SIGNING KEY -- this artifact is UNSIGNED and " +
                        "Android will refuse to install it.\n" +
                        "  Do not sideload the debug APK instead: it is debuggable and signed with " +
                        "the stock 'CN=Android Debug' key.\n" +
                        "  Fix: cp packaging/android/keystore.properties.example packaging/android/keystore.properties " +
                        "and follow the keytool command in it.\n"
                )
            }
        }
    }
}

// The staged libraries live under the build directory now, so `clean` already
// removes them. What is left to clean is the *old* shared location, which a
// pre-fix checkout may still have on disk.
//
// This is `delete(...)` on the Delete task rather than the `doLast { … }` that
// used to be here, and the difference is not cosmetic: that block deleted a
// directory the cargo-ndk tasks write, with no ordering constraint between them,
// so `./gradlew clean assembleDebug` in one invocation could stage the library
// and then delete it -- or not -- depending on task scheduling. Declaring it as
// an input to `clean` lets Gradle see the relationship instead of racing it. No
// task writes to this path any more, so there is nothing left to race.
tasks.named<Delete>("clean").configure {
    delete(legacyJniLibsDir)
}

dependencies {
    // The Kotlin CertificateVerifier the Rust side reaches over JNI. The version
    // is whatever the resolved `rustls-platform-verifier-android` crate bundles,
    // so it tracks the Cargo dependency rather than being pinned in two places.
    //
    // kotlin-stdlib, which this needs at runtime, is not declared in the AAR's POM
    // and does not have to be declared here either: AGP 9's built-in Kotlin
    // support puts it on the runtime classpath. That is precisely the manual
    // kotlin-stdlib download build-android.sh used to do before calling d8.
    implementation("rustls:rustls-platform-verifier:${rustlsVerifier.version}")
}
