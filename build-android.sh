#!/bin/bash

# Build script for Android APK
# Prerequisites: 
# - cargo-apk installed (cargo install cargo-apk)
# - Android SDK and NDK installed
# - ANDROID_HOME environment variable set

set -e

echo "🤖 Building Rustdar Platform for Android..."

# Check if cargo-apk is installed
if ! command -v cargo-apk &> /dev/null; then
    echo "❌ cargo-apk is not installed. Install it with: cargo install cargo-apk"
    exit 1
fi

# Check if ANDROID_HOME is set
if [ -z "$ANDROID_HOME" ]; then
    echo "❌ ANDROID_HOME environment variable is not set"
    echo "Please set it to your Android SDK path, e.g.:"
    echo "export ANDROID_HOME=/home/user/Android/Sdk"
    exit 1
fi

# Navigate to the Android crate directory
cd rustdar-android

echo "🏗️  Building APK..."

# Build the APK without default features (which includes the native binary)
cargo apk build --no-default-features

echo "✅ APK built successfully!"
echo "📦 APK location: target/android-artifacts/release/apk/"

# List the built APK files
echo "📱 Built APKs:"
find target/android-artifacts/release/apk/ -name "*.apk" -type f 2>/dev/null || echo "No APK files found"

echo ""
echo "🚀 To install on device:"
echo "adb install target/android-artifacts/release/apk/rustdar-platform.apk"
echo ""
echo "📋 To view logs:"
echo "adb logcat -s rustdar"