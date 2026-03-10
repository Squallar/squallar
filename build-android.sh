#!/bin/bash

# Build script for Android APK
# Prerequisites:
# - cargo-apk installed (cargo install cargo-apk)
# - Android SDK and NDK installed
# - ANDROID_HOME environment variable set

set -euo pipefail

echo "Building Rustdar Platform for Android..."

# ---------- Preflight checks ----------

if ! command -v cargo-apk &> /dev/null; then
    echo "ERROR: cargo-apk is not installed. Install it with: cargo install cargo-apk"
    exit 1
fi

if [ -z "$ANDROID_HOME" ]; then
    echo "ERROR: ANDROID_HOME environment variable is not set"
    echo "Please set it to your Android SDK path, e.g.:"
    echo "  export ANDROID_HOME=/home/user/Android/Sdk"
    exit 1
fi

# Find the latest build-tools version for d8, zipalign, apksigner
BUILD_TOOLS_DIR=$(ls -d "$ANDROID_HOME/build-tools/"*/ 2>/dev/null | sort -V | tail -1)
if [ -z "$BUILD_TOOLS_DIR" ]; then
    echo "ERROR: No Android build-tools found under $ANDROID_HOME/build-tools/"
    exit 1
fi
echo "Using build-tools: $BUILD_TOOLS_DIR"

D8="$BUILD_TOOLS_DIR/d8"
ZIPALIGN="$BUILD_TOOLS_DIR/zipalign"
APKSIGNER="$BUILD_TOOLS_DIR/apksigner"

for tool in "$D8" "$ZIPALIGN" "$APKSIGNER"; do
    if [ ! -x "$tool" ]; then
        echo "ERROR: Required tool not found: $tool"
        exit 1
    fi
done

# ---------- Step 1: cargo apk build ----------

cd rustdar-android
echo "[1/4] Building native APK with cargo-apk..."
cargo apk build --no-default-features
cd ..

APK_DIR="target/debug/apk"

# Derive APK filename from the lib name in Cargo.toml (cargo-apk uses this)
LIB_NAME=$(grep -A5 '^\[lib\]' rustdar-android/Cargo.toml | grep '^name' | head -1 | sed 's/.*= *"\(.*\)"/\1/')
if [ -z "$LIB_NAME" ]; then
    LIB_NAME="rustdar_android"
fi
APK="$APK_DIR/$LIB_NAME.apk"
UNALIGNED_APK="$APK_DIR/$LIB_NAME-unaligned.apk"

if [ ! -f "$UNALIGNED_APK" ]; then
    echo "ERROR: cargo-apk did not produce $UNALIGNED_APK"
    exit 1
fi

# ---------- Step 2: Extract & DEX-ify the platform verifier Kotlin component ----------

echo "[2/4] Injecting rustls-platform-verifier Kotlin component..."

# Locate the AAR bundled inside the rustls-platform-verifier-android crate
AAR=$(find "$HOME/.cargo/registry/src" -path "*/rustls-platform-verifier-android-*/maven/rustls/rustls-platform-verifier/*/rustls-platform-verifier-*.aar" 2>/dev/null | sort -V | tail -1)

if [ -z "$AAR" ]; then
    echo "ERROR: Could not find rustls-platform-verifier AAR in cargo registry."
    echo "Make sure the rustls-platform-verifier crate has been downloaded (cargo build first)."
    exit 1
fi
echo "  Found AAR: $AAR"

WORK_DIR=$(mktemp -d)
trap "rm -rf $WORK_DIR" EXIT

# Extract classes.jar from the AAR (AAR is a ZIP)
unzip -q -o "$AAR" classes.jar -d "$WORK_DIR"
if [ ! -f "$WORK_DIR/classes.jar" ]; then
    echo "ERROR: classes.jar not found inside $AAR"
    exit 1
fi

# The CertificateVerifier is written in Kotlin and depends on kotlin-stdlib.
# Download it (cached) so we can include it in the DEX.
KOTLIN_VERSION="1.9.25"
KOTLIN_CACHE_DIR="$HOME/.cache/rustdar-build"
KOTLIN_STDLIB="$KOTLIN_CACHE_DIR/kotlin-stdlib-$KOTLIN_VERSION.jar"

if [ ! -f "$KOTLIN_STDLIB" ]; then
    echo "  Downloading kotlin-stdlib $KOTLIN_VERSION..."
    mkdir -p "$KOTLIN_CACHE_DIR"
    curl -sL "https://repo1.maven.org/maven2/org/jetbrains/kotlin/kotlin-stdlib/$KOTLIN_VERSION/kotlin-stdlib-$KOTLIN_VERSION.jar" \
        -o "$KOTLIN_STDLIB.tmp"
    mv "$KOTLIN_STDLIB.tmp" "$KOTLIN_STDLIB"
fi
echo "  kotlin-stdlib: $KOTLIN_STDLIB"

# Compile our Java helper classes (e.g. BackHandler for Android 13+ back gesture)
echo "  Compiling Java helpers..."
JAVA_SRC_DIR="rustdar-android/java"
JAVA_OUT_DIR="$WORK_DIR/java_classes"
mkdir -p "$JAVA_OUT_DIR"

# Find a suitable android.jar (API 33+ needed for OnBackInvokedCallback)
ANDROID_JAR=""
for api in 35 34 33; do
    candidate="$ANDROID_HOME/platforms/android-$api/android.jar"
    if [ -f "$candidate" ]; then
        ANDROID_JAR="$candidate"
        break
    fi
done
if [ -z "$ANDROID_JAR" ]; then
    echo "WARNING: No android.jar (API 33+) found; BackHandler will not be compiled."
    echo "         Back gesture may not work on Android 13+."
else
    echo "  android.jar: $ANDROID_JAR"
    javac -source 8 -target 8 \
        -classpath "$ANDROID_JAR" \
        -d "$JAVA_OUT_DIR" \
        $(find "$JAVA_SRC_DIR" -name '*.java') 2>/dev/null || true
fi

# Compile JARs + our helper classes into a single classes.dex (min-api 26 matches our minSdkVersion)
echo "  Running d8..."
HELPER_CLASSES=$(find "$JAVA_OUT_DIR" -name '*.class' 2>/dev/null)
"$D8" --min-api 26 --output "$WORK_DIR" "$WORK_DIR/classes.jar" "$KOTLIN_STDLIB" $HELPER_CLASSES
if [ ! -f "$WORK_DIR/classes.dex" ]; then
    echo "ERROR: d8 failed to produce classes.dex"
    exit 1
fi
echo "  classes.dex: $(wc -c < "$WORK_DIR/classes.dex") bytes"

# ---------- Step 3: Inject DEX into APK, re-align, re-sign ----------

echo "[3/4] Merging DEX into APK and re-signing..."

# cargo-apk produces an unaligned APK. We inject the DEX into that,
# then zipalign and sign ourselves (replacing the cargo-apk signed copy).
# Remove the old signature first so apksigner can re-sign cleanly.
cp "$UNALIGNED_APK" "$WORK_DIR/rustdar.apk"

# Add classes.dex into the APK (zip -j = junk paths, store at root)
(cd "$WORK_DIR" && zip -q rustdar.apk classes.dex)

# zipalign
"$ZIPALIGN" -f 4 "$WORK_DIR/rustdar.apk" "$WORK_DIR/rustdar-aligned.apk"

# Generate a debug keystore if one doesn't exist
DEBUG_KEYSTORE="$HOME/.android/debug.keystore"
if [ ! -f "$DEBUG_KEYSTORE" ]; then
    echo "  Generating debug keystore..."
    mkdir -p "$HOME/.android"
    keytool -genkey -v -keystore "$DEBUG_KEYSTORE" \
        -storepass android -keypass android \
        -alias androiddebugkey \
        -keyalg RSA -keysize 2048 -validity 10000 \
        -dname "CN=Android Debug,O=Android,C=US" 2>/dev/null
fi

# Sign with debug key
"$APKSIGNER" sign \
    --ks "$DEBUG_KEYSTORE" \
    --ks-key-alias androiddebugkey \
    --ks-pass pass:android \
    --key-pass pass:android \
    "$WORK_DIR/rustdar-aligned.apk"

# Replace the output APK
cp "$WORK_DIR/rustdar-aligned.apk" "$APK"
echo "  DEX injected and APK re-signed."

# ---------- Done ----------

echo "[4/4] Done!"
echo ""
echo "APK: $APK"
echo ""
echo "Install:  adb install -r $APK"
echo "Logs:     adb logcat -s rustdar"