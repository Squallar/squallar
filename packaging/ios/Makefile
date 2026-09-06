# iOS app build, run entirely on Linux inside the rust-cross container:
#
#   make -C packaging/ios                 device   (aarch64-apple-ios),     release -> .app + .ipa
#   make -C packaging/ios SIMULATOR=1     simulator(aarch64-apple-ios-sim), release -> .app
#   make -C packaging/ios DEBUG=1         debug profile
#
# Output is UNSIGNED. Installing on a device needs `rcodesign`; the simulator
# does not care. No Apple tools and no macOS host are involved -- the .app is
# assembled with cp/sed/zip.
#
# CI already runs *inside* the image, so it sets IN_CONTAINER=1 and the same
# recipe executes directly instead of nesting a second container.
#
# The app is a thin UIKit shell (packaging/ios/Sources/main.m) linking the Rust staticlib
# and calling `squallar_ios_main`. Bundle metadata is read out of packaging/ios/project.yml
# so Xcode and this Makefile cannot disagree about it.
SHELL := bash
.SHELLFLAGS := -euo pipefail -c
.ONESHELL:
ROOT := $(realpath $(CURDIR)/../..)

# Digest-pinned to the same image the CI matrix uses.
IMAGE ?= ghcr.io/usa-reddragon/rust-cross:1.97.1@sha256:68d608cfe4232a8a9b42ce41ea26412eaadc8f53218fed7e52e43974b388a302
# Named volume for the crate cache, so a rebuild does not re-download the index.
CARGO_VOL ?= squallar-xcross-cargo
# Must match MinimumOSVersion in packaging/ios/Info.plist; it is what drives the clang
# wrapper's -target/-mios-version-min and so the binary's LC_BUILD_VERSION `minos`.
#
# It used to drive the `sdk` field too, and that was a defect. The image's
# clang wrapper is
#
#   clang -target "arm64-apple-ios$V" -isysroot "$IOS_SDKROOT" -mios-version-min="$V" ...
#
# and nothing in it tells the linker which SDK the link actually used, so ld64
# recorded the deployment target in both halves of LC_BUILD_VERSION: `sdk 14.0`
# beside `minos 14.0`, while the bundle's DTSDKName -- read out of the SDK
# itself -- said `iphoneos26.4`. The two fields described different SDKs.
#
# That is invisible to a device install, which is why it survived: iOS reads
# `minos` to decide whether the binary will run, and nothing on the device
# looks at `sdk`. App Store Connect does. Apple enforces a recent-SDK floor on
# upload and reads exactly this field, so a build carrying `sdk 14.0` is the
# "built with an outdated SDK" rejection -- and until this pipeline could
# upload, it had no consumer that could tell.
#
# The fix is one linker flag, and the number has a source: SDKSettings.json in
# the SDK the link just used carries `Version` (26.4) beside the
# `CanonicalName` (iphoneos26.4) that DTSDKName is already read from. Both are
# extracted before the link and the guard further down asserts the three
# spellings agree, so this cannot drift from the image again.
#
# `-Wl,-sdk_version` rather than `-Wl,-platform_version,ios,<min>,<sdk>`:
# measured against this image's ld64-711, both write `sdk 26.4`, but the
# platform_version spelling collides with the -mios-version-min the wrapper
# already passes and ld warns "passed two min versions (14.0, 14.0)". Same
# result, one spelling has no warning to explain to the next reader.
IOS_MIN ?= 14.0
# Reads LC_BUILD_VERSION back out of the linked binary. cctools' own `otool`
# and `vtool` are not in this image; llvm-objdump is, and its --macho reader
# prints the same fields. Pinned by absolute path because it is not on PATH.
OBJDUMP ?= /opt/llvm-mingw/bin/llvm-objdump
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
	# putting `staticlib` in squallar's [lib] would archive the whole
	# graph on every desktop build and every --all-targets clippy run too.
	#
	# The framework list mirrors packaging/ios/project.yml. Nothing here is auto-linked:
	# a link without `-framework Security` fails on SecTrustEvaluateWithError,
	# which is rustls-platform-verifier's Apple trust path, and one without
	# `-framework CoreLocation` fails on the CLLocationManager class reference
	# that squallar's os_location/apple.rs emits.
	INCMD="set -e
	    export RUSTUP_TOOLCHAIN=\"\$${RUST_VERSION:?the rust-cross image must set RUST_VERSION}\"
	    export IPHONEOS_DEPLOYMENT_TARGET=$(IOS_MIN)
	    export CARGO_TARGET_DIR=$(TARGET_DIR)
	    cargo rustc $$CARGO_PROFILE --target $$RUST_TARGET -p squallar --lib --crate-type staticlib
	    test -f $$LIBDIR/libsquallar_native.a || { echo 'staticlib missing'; exit 1; }
	    $$IOS_CC -c packaging/ios/Sources/main.m -o $$LIBDIR/squallar_main.o
	    tr -d '\"' < $$SDKROOT_REF/SDKSettings.json | tr ',' '\n' \
	        | sed -n 's/^CanonicalName://p' | head -1 > $$LIBDIR/sdk-name
	    tr -d '\"' < $$SDKROOT_REF/SDKSettings.json | tr ',' '\n' \
	        | sed -n 's/^Version://p' | head -1 > $$LIBDIR/sdk-version
	    $$IOS_CC $$LIBDIR/squallar_main.o $$LIBDIR/libsquallar_native.a \
	        -lobjc \
	        -framework UIKit -framework QuartzCore -framework Metal \
	        -framework CoreGraphics \
	        -framework Foundation -framework CoreFoundation \
	        -framework Security -framework CoreLocation \
	        -Wl,-sdk_version,\$$(cat $$LIBDIR/sdk-version) \
	        -o $$LIBDIR/squallar
	    test -x $(OBJDUMP) || { echo 'ERROR: $(OBJDUMP) is not in this image, so the SDK-version guard below cannot read the binary it is supposed to check.'; exit 1; }
	    $(OBJDUMP) --macho --all-headers $$LIBDIR/squallar \
	        | sed -n '/LC_BUILD_VERSION/,/ntools/p' > $$LIBDIR/build-version"

	if [ -n "$(IN_CONTAINER)" ]; then
	  echo "==> cargo + link ($$RUST_TARGET, $$PROFILE_DIR)"
	  sh -c "$$INCMD"
	else
	  ENGINE="$${SQUALLAR_CONTAINER_ENGINE:-}"
	  if [ -z "$$ENGINE" ]; then
	    if command -v podman >/dev/null 2>&1; then ENGINE=podman
	    elif command -v docker >/dev/null 2>&1; then ENGINE=docker
	    else echo "ERROR: need podman or docker (set SQUALLAR_CONTAINER_ENGINE, or IN_CONTAINER=1 if already inside the image)."; exit 1; fi
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
	[ -f "$$LIBDIR/squallar" ] || { echo "ERROR: link produced no executable at $$LIBDIR/squallar"; exit 1; }

	# ---- assemble the .app (plain file ops, no Apple tools) ----
	echo "==> assemble squallar.app"
	# Single source of bundle metadata: packaging/ios/project.yml.
	yaml_val() { sed -n "s/.*$$1: *\"\{0,1\}\([^\"]*\)\"\{0,1\}.*/\1/p" packaging/ios/project.yml | head -1; }
	BUNDLE_ID="$$(yaml_val PRODUCT_BUNDLE_IDENTIFIER)"
	MARKETING_VERSION="$$(yaml_val MARKETING_VERSION)"
	CURRENT_PROJECT_VERSION="$$(yaml_val CURRENT_PROJECT_VERSION)"
	: "$${BUNDLE_ID:?could not read PRODUCT_BUNDLE_IDENTIFIER from packaging/ios/project.yml}"
	# DTSDKName is read off the SDK the link just used rather than written down
	# here, so it cannot drift from the image.
	SDK_NAME="$$(cat "$$LIBDIR/sdk-name")"
	: "$${SDK_NAME:?no CanonicalName in the SDKSettings.json of the linked SDK}"
	SDK_VERSION="$$(cat "$$LIBDIR/sdk-version")"
	: "$${SDK_VERSION:?no Version in the SDKSettings.json of the linked SDK}"

	# ---- the three spellings of "which SDK built this" must agree ----
	# The bundle says one (DTSDKName, from CanonicalName), the Mach-O says
	# another (LC_BUILD_VERSION sdk, written by the linker), and the SDK itself
	# says a third (SDKSettings.json Version). They came from one directory, so
	# a disagreement is a defect in this recipe, not a fact about the SDK.
	#
	# This runs on every build, needs no Apple account and no device, and it is
	# the only thing standing between a wrong `sdk` field and an App Store
	# Connect rejection that costs a round trip to discover. See the note on
	# IOS_MIN at the top for what the wrong value used to be and why nothing
	# noticed.
	MACHO_SDK="$$(sed -n 's/^ *sdk *//p' "$$LIBDIR/build-version" | head -1)"
	MACHO_MINOS="$$(sed -n 's/^ *minos *//p' "$$LIBDIR/build-version" | head -1)"
	if [ -z "$$MACHO_SDK" ] || [ -z "$$MACHO_MINOS" ]; then
	  echo "ERROR: no LC_BUILD_VERSION sdk/minos could be read back out of the linked"
	  echo "       binary. Either the link did not write the load command or the"
	  echo "       objdump output changed shape; in both cases this guard has"
	  echo "       stopped checking anything. Dump follows:"
	  sed 's/^/       /' "$$LIBDIR/build-version" || true
	  exit 1
	fi
	if [ "$$MACHO_SDK" != "$$SDK_VERSION" ]; then
	  echo "ERROR: the binary records LC_BUILD_VERSION sdk $$MACHO_SDK, but it was"
	  echo "       linked against the $$SDK_VERSION SDK. App Store Connect reads this"
	  echo "       field and enforces a floor on it, so this build would be rejected"
	  echo "       on upload as 'built with an outdated SDK'."
	  exit 1
	fi
	if [ "$$MACHO_MINOS" != "$(IOS_MIN)" ]; then
	  echo "ERROR: the binary records LC_BUILD_VERSION minos $$MACHO_MINOS, but this"
	  echo "       Makefile asked for $(IOS_MIN). MinimumOSVersion in the bundle will"
	  echo "       claim one thing and the loader enforce another."
	  exit 1
	fi
	if [ "$$SDK_NAME" != "$$SDK$$SDK_VERSION" ]; then
	  echo "ERROR: the SDK's CanonicalName is $$SDK_NAME, which is not $$SDK followed"
	  echo "       by its Version ($$SDK_VERSION). DTSDKName and the Mach-O's sdk"
	  echo "       field are both derived from this SDK on the assumption that they"
	  echo "       are the same number, and here they are not."
	  exit 1
	fi
	echo "    SDK: DTSDKName=$$SDK_NAME, LC_BUILD_VERSION sdk=$$MACHO_SDK minos=$$MACHO_MINOS"

	OUT="packaging/ios/build/linux/$$SDK"; APP="$$OUT/squallar.app"
	rm -rf "$$OUT"; mkdir -p "$$APP"
	cp "$$LIBDIR/squallar" "$$APP/squallar"; chmod +x "$$APP/squallar"
	# Resolve the Xcode build-setting variables xcodebuild would have expanded.
	sed -e "s|\$$(EXECUTABLE_NAME)|squallar|g" \
	    -e "s|\$$(PRODUCT_BUNDLE_IDENTIFIER)|$$BUNDLE_ID|g" \
	    -e "s|\$$(MARKETING_VERSION)|$$MARKETING_VERSION|g" \
	    -e "s|\$$(CURRENT_PROJECT_VERSION)|$$CURRENT_PROJECT_VERSION|g" \
	    -e "s|\$$(SUPPORTED_PLATFORM)|$$SUPPORTED_PLATFORM|g" \
	    -e "s|\$$(PLATFORM_NAME)|$$SDK|g" \
	    -e "s|\$$(SDK_NAME)|$$SDK_NAME|g" \
	    packaging/ios/Info.plist > "$$APP/Info.plist"
	# An unexpanded $(...) here means a key silently ships as a literal.
	! grep -q '\$$(' "$$APP/Info.plist" || { echo "ERROR: unexpanded build setting left in Info.plist"; exit 1; }
	# iPhone presentation clamps to 60 Hz on ProMotion panels without this key;
	# iPads are exempt.
	grep -A1 '<key>CADisableMinimumFrameDurationOnPhone</key>' "$$APP/Info.plist" | grep -q '<true/>' \
	    || { echo "ERROR: CADisableMinimumFrameDurationOnPhone is not true in the bundled Info.plist"; exit 1; }
	echo ""
	if [ "$$SDK" = iphonesimulator ]; then
	  echo "App: $(ROOT)/$$APP  (simulator, arm64)"
	  echo "Run on an Apple Silicon macOS host:"
	  echo "  xcrun simctl boot 'iPhone 15' || true"
	  echo "  xcrun simctl install booted '$$APP'"
	  echo "  xcrun simctl launch --console booted $$BUNDLE_ID"
	elif command -v zip >/dev/null 2>&1; then
	  # (Unsigned) .ipa: Payload/squallar.app zipped at the archive root.
	  rm -rf "$$OUT/Payload"; mkdir -p "$$OUT/Payload"
	  cp -R "$$APP" "$$OUT/Payload/"
	  ( cd "$$OUT" && zip -qr squallar.ipa Payload && rm -rf Payload )
	  echo "App: $(ROOT)/$$APP  (UNSIGNED)"
	  echo "IPA: $(ROOT)/$$OUT/squallar.ipa  (UNSIGNED)"
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
	@rm -rf "$(ROOT)/packaging/ios/build" "$(ROOT)/$(TARGET_DIR)"
