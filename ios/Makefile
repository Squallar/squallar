# iOS app build, run entirely on Linux inside the rust-cross container:
#
#   make -C ios                 device   (aarch64-apple-ios),     release -> .app + .ipa
#   make -C ios SIMULATOR=1     simulator(aarch64-apple-ios-sim), release -> .app
#   make -C ios DEBUG=1         debug profile
#
# Output is UNSIGNED. Installing on a device needs `rcodesign`; the simulator
# does not care. No Apple tools and no macOS host are involved -- the .app is
# assembled with cp/sed/zip.
#
# CI already runs *inside* the image, so it sets IN_CONTAINER=1 and the same
# recipe executes directly instead of nesting a second container.
#
# The app is a thin UIKit shell (ios/Sources/main.m) linking the Rust staticlib
# and calling `rustdar_ios_main`. Bundle metadata is read out of ios/project.yml
# so Xcode and this Makefile cannot disagree about it.
SHELL := bash
.SHELLFLAGS := -euo pipefail -c
.ONESHELL:
ROOT := $(realpath $(CURDIR)/..)

# Digest-pinned to the same image the CI matrix uses.
IMAGE ?= ghcr.io/usa-reddragon/rust-cross:1.97.1@sha256:433d3e61c46c6dea2e0bb0bc450fe34415d9769f6d801ab1f2e2ebbdbcf50c28
# Named volume for the crate cache, so a rebuild does not re-download the index.
CARGO_VOL ?= rustdar-xcross-cargo
# Must match MinimumOSVersion in ios/Info.plist; it is what drives the clang
# wrapper's -target/-mios-version-min and so the binary's LC_BUILD_VERSION.
IOS_MIN ?= 14.0
# Separate from target/, which holds host artifacts: one directory shared by two
# toolchains with two file ownerships is how a stale-lock stall starts.
TARGET_DIR ?= target-ios

.PHONY: build clean
build:
	@cd "$(ROOT)"
	SDK=iphoneos; RUST_TARGET=aarch64-apple-ios; IOS_CC=arm64-apple-ios-clang
	# CFBundleSupportedPlatforms is capitalised where PLATFORM_NAME is not, and
	# a bundle claiming the wrong one is what "not compatible" means to simctl.
	SUPPORTED_PLATFORM=iPhoneOS; SDKROOT_REF='$$IOS_SDKROOT'
	if [ -n "$(SIMULATOR)" ]; then
	  SDK=iphonesimulator; RUST_TARGET=aarch64-apple-ios-sim; IOS_CC=arm64-apple-ios-sim-clang
	  SUPPORTED_PLATFORM=iPhoneSimulator; SDKROOT_REF='$$IOS_SIMULATOR_SDKROOT'
	fi
	if [ -n "$(DEBUG)" ]; then PROFILE_DIR=debug; CARGO_PROFILE=; else PROFILE_DIR=release; CARGO_PROFILE=--release; fi
	LIBDIR="$(TARGET_DIR)/$$RUST_TARGET/$$PROFILE_DIR"

	# RUSTUP_TOOLCHAIN from the image's own RUST_VERSION: rust-toolchain.toml
	# says `stable`, and obeying it here fetches a toolchain that has none of
	# the image's 23 preinstalled targets.
	#
	# `cargo rustc --crate-type staticlib` rather than a manifest crate-type:
	# putting `staticlib` in rustdar-platform's [lib] would archive the whole
	# graph on every desktop build and every --all-targets clippy run too.
	#
	# The framework list mirrors ios/project.yml. Nothing here is auto-linked:
	# a link without `-framework Security` fails on SecTrustEvaluateWithError,
	# which is rustls-platform-verifier's Apple trust path, and one without
	# `-framework CoreLocation` fails on the CLLocationManager class reference
	# that rustdar-platform's os_location/apple.rs emits.
	INCMD="set -e
	    export RUSTUP_TOOLCHAIN=\"\$${RUST_VERSION:?the rust-cross image must set RUST_VERSION}\"
	    export IPHONEOS_DEPLOYMENT_TARGET=$(IOS_MIN)
	    export CARGO_TARGET_DIR=$(TARGET_DIR)
	    cargo rustc $$CARGO_PROFILE --target $$RUST_TARGET -p rustdar-platform --lib --crate-type staticlib
	    test -f $$LIBDIR/librustdar_platform_lib.a || { echo 'staticlib missing'; exit 1; }
	    $$IOS_CC -c ios/Sources/main.m -o $$LIBDIR/rustdar_main.o
	    $$IOS_CC $$LIBDIR/rustdar_main.o $$LIBDIR/librustdar_platform_lib.a \
	        -lobjc \
	        -framework UIKit -framework QuartzCore -framework Metal \
	        -framework CoreGraphics \
	        -framework Foundation -framework CoreFoundation \
	        -framework Security -framework CoreLocation \
	        -o $$LIBDIR/rustdar
	    tr -d '\"' < $$SDKROOT_REF/SDKSettings.json | tr ',' '\n' \
	        | sed -n 's/^CanonicalName://p' | head -1 > $$LIBDIR/sdk-name"

	if [ -n "$(IN_CONTAINER)" ]; then
	  echo "==> cargo + link ($$RUST_TARGET, $$PROFILE_DIR)"
	  sh -c "$$INCMD"
	else
	  ENGINE="$${RUSTDAR_CONTAINER_ENGINE:-}"
	  if [ -z "$$ENGINE" ]; then
	    if command -v podman >/dev/null 2>&1; then ENGINE=podman
	    elif command -v docker >/dev/null 2>&1; then ENGINE=docker
	    else echo "ERROR: need podman or docker (set RUSTDAR_CONTAINER_ENGINE, or IN_CONTAINER=1 if already inside the image)."; exit 1; fi
	  fi
	  echo "==> [$$ENGINE] cargo + link ($$RUST_TARGET, $$PROFILE_DIR)"
	  # Rootless podman already maps container root to the invoking user, so
	  # chowning there would push the files onto a subuid instead.
	  if [ "$$ENGINE" = docker ]; then INCMD="$$INCMD
	    chown -R $$(id -u):$$(id -g) $(TARGET_DIR)"; fi
	  "$$ENGINE" run --rm \
	      -v "$(ROOT)":/project -w /project \
	      -v "$(CARGO_VOL)":/usr/local/cargo/registry \
	      "$(IMAGE)" sh -c "$$INCMD"
	fi
	[ -f "$$LIBDIR/rustdar" ] || { echo "ERROR: link produced no executable at $$LIBDIR/rustdar"; exit 1; }

	# ---- assemble the .app (plain file ops, no Apple tools) ----
	echo "==> assemble rustdar.app"
	# Single source of bundle metadata: ios/project.yml.
	yaml_val() { sed -n "s/.*$$1: *\"\{0,1\}\([^\"]*\)\"\{0,1\}.*/\1/p" ios/project.yml | head -1; }
	BUNDLE_ID="$$(yaml_val PRODUCT_BUNDLE_IDENTIFIER)"
	MARKETING_VERSION="$$(yaml_val MARKETING_VERSION)"
	CURRENT_PROJECT_VERSION="$$(yaml_val CURRENT_PROJECT_VERSION)"
	: "$${BUNDLE_ID:?could not read PRODUCT_BUNDLE_IDENTIFIER from ios/project.yml}"
	# DTSDKName is read off the SDK the link just used rather than written down
	# here, so it cannot drift from the image.
	SDK_NAME="$$(cat "$$LIBDIR/sdk-name")"
	: "$${SDK_NAME:?no CanonicalName in the SDKSettings.json of the linked SDK}"

	OUT="ios/build/linux/$$SDK"; APP="$$OUT/rustdar.app"
	rm -rf "$$OUT"; mkdir -p "$$APP"
	cp "$$LIBDIR/rustdar" "$$APP/rustdar"; chmod +x "$$APP/rustdar"
	# Resolve the Xcode build-setting variables xcodebuild would have expanded.
	sed -e "s|\$$(EXECUTABLE_NAME)|rustdar|g" \
	    -e "s|\$$(PRODUCT_BUNDLE_IDENTIFIER)|$$BUNDLE_ID|g" \
	    -e "s|\$$(MARKETING_VERSION)|$$MARKETING_VERSION|g" \
	    -e "s|\$$(CURRENT_PROJECT_VERSION)|$$CURRENT_PROJECT_VERSION|g" \
	    -e "s|\$$(SUPPORTED_PLATFORM)|$$SUPPORTED_PLATFORM|g" \
	    -e "s|\$$(PLATFORM_NAME)|$$SDK|g" \
	    -e "s|\$$(SDK_NAME)|$$SDK_NAME|g" \
	    ios/Info.plist > "$$APP/Info.plist"
	# An unexpanded $(...) here means a key silently ships as a literal.
	! grep -q '\$$(' "$$APP/Info.plist" || { echo "ERROR: unexpanded build setting left in Info.plist"; exit 1; }
	echo ""
	if [ "$$SDK" = iphonesimulator ]; then
	  echo "App: $(ROOT)/$$APP  (simulator, arm64)"
	  echo "Run on an Apple Silicon macOS host:"
	  echo "  xcrun simctl boot 'iPhone 15' || true"
	  echo "  xcrun simctl install booted '$$APP'"
	  echo "  xcrun simctl launch --console booted $$BUNDLE_ID"
	elif command -v zip >/dev/null 2>&1; then
	  # (Unsigned) .ipa: Payload/rustdar.app zipped at the archive root.
	  rm -rf "$$OUT/Payload"; mkdir -p "$$OUT/Payload"
	  cp -R "$$APP" "$$OUT/Payload/"
	  ( cd "$$OUT" && zip -qr rustdar.ipa Payload && rm -rf Payload )
	  echo "App: $(ROOT)/$$APP  (UNSIGNED)"
	  echo "IPA: $(ROOT)/$$OUT/rustdar.ipa  (UNSIGNED)"
	  echo "Sign for a device with rcodesign, e.g.:"
	  echo "  rcodesign sign --p12-file dev.p12 --code-signature-flags runtime \\"
	  echo "    --entitlements-xml-path ent.plist $$APP"
	else
	  # The rust-cross image has no zip. The .app is the artifact that matters;
	  # the .ipa is only Payload/ in a zip, so skipping it loses no coverage.
	  echo "App: $(ROOT)/$$APP  (UNSIGNED)"
	  echo "note: no 'zip' on PATH, so no .ipa was packaged."
	fi

clean:
	@rm -rf "$(ROOT)/ios/build" "$(ROOT)/$(TARGET_DIR)"
