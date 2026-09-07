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
#
# ── Signing for the App Store, and why this is not the usual answer ─────
#
# With `IOS_SIGNING_P12` and `IOS_PROVISIONING_PROFILE` set, this Makefile also
# signs the bundle for App Store distribution and names the archive
# `squallar.ipa`. Without them it names it `squallar-UNSIGNED.ipa` and says so.
# One name means uploadable; every other outcome means not.
#
# The upload itself is .github/scripts/appstore-upload.py, and it runs on Linux
# because App Store Connect API 4.1 made that possible: the `buildUploads` and
# `buildUploadFiles` resources are a REST flow -- reserve, PUT, commit, poll --
# with an ES256 bearer token. The tools people reach for first cannot help
# here. `xcrun altool` and `notarytool` are inside Xcode and macOS-only, and
# Apple's Transporter, which does have a Red Hat build, refuses on Linux with
# "Unable to perform software analysis on Linux. Export an AppStoreInfo.plist
# from Xcode, and use the -assetDescription option". The REST path needs none
# of it.
#
# ── The three signing secrets ───────────────────────────────────────────
#
#   IOS_SIGNING_P12            base64 of a .p12 holding the Apple Distribution
#                              certificate and its private key
#   IOS_SIGNING_P12_PASSWORD   its password
#   IOS_PROVISIONING_PROFILE   base64 of the App Store .mobileprovision
#
# and three more, read only by the upload script and only on a push to main:
#
#   APPSTORE_CONNECT_ISSUER_ID App Store Connect issuer ID (a UUID)
#   APPSTORE_CONNECT_KEY_ID    API key ID (10 alphanumeric characters)
#   APPSTORE_CONNECT_KEY_P8    the AuthKey_<KEY_ID>.p8 Apple hands out ONCE --
#                              either `base64 -w0` of it, or the PEM text
#
# The last three are the SAME secrets packaging/macos/Makefile notarizes with:
# one App Store Connect key, role App Manager, set once. Two copies of one key
# under two names is a rotation hazard, and a fallback from one name to the
# other would leave "which key did this run use?" unanswerable, so the earlier
# APPSTORE_* and MACOS_NOTARY_* spellings are gone rather than aliased.
#
# All of the first three, or none. Locally, `P12_FILE=`, `P12_PASSWORD=` and
# `PROFILE_FILE=` take paths instead.
#
# ── What a person has to do once the Apple Developer account is approved ─
#
#   1. Make a key, then a signing request from it. Apple never sees the
#      private key:
#
#        openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 \
#            -out squallar-ios-key.pem
#        rcodesign generate-certificate-signing-request \
#            --pem-file squallar-ios-key.pem \
#            --csr-pem-file squallar-ios.csr
#
#      `--pem-file` READS a key; it does not write one. Measured with 0.29.0:
#      run without the genpkey line first, it exits with "I/O error: No such
#      file or directory" and writes no CSR.
#
#   2. developer.apple.com -> Certificates, IDs & Profiles -> Certificates ->
#      "+" -> **Apple Distribution**. NOT "Apple Development", which signs
#      builds for machines on the team, and NOT the "Developer ID Application"
#      certificate that already exists for the macOS lane -- that one is for
#      software handed out directly and the App Store will not take it. Upload
#      the CSR, download the .cer, and combine that LEAF and the key -- those
#      two and nothing else -- into one .p12, with `-legacy`:
#
#        openssl x509 -inform DER -in distribution.cer -out distribution.pem
#        openssl pkcs12 -export -legacy \
#            -inkey squallar-ios-key.pem -in distribution.pem \
#            -out squallar-ios.p12
#        base64 -w0 squallar-ios.p12    -> IOS_SIGNING_P12
#                                       -> IOS_SIGNING_P12_PASSWORD
#
#      Two ways a correct certificate under a correct password still fails,
#      both decided by how `pkcs12 -export` is invoked, and both checked by
#      the signing stage below before rcodesign sees the file:
#
#      No `-certfile`, no `-chain`. rcodesign signs with the FIRST
#      certificate in the file, and OpenSSL writes a `-certfile` CA before
#      the leaf. Measured with 0.29.0: a throwaway CA with OU=G2 in front of
#      a leaf with OU=ABCDE12345 made `analyze-certificate` report
#      Certificate 0 with Team ID G2 and made `sign` emit a signature whose
#      certificate was the CA; the same leaf and key alone reported
#      ABCDE12345. The intermediates are not needed in the file: rcodesign
#      ships Apple's CA certificates (src/apple-certs/AppleWWDRCAG3.cer among
#      them) and adds the chain from the leaf upward when it loads the key
#      (chain_apple_certificates); `analyze-certificate` on a bare
#      Apple-issued leaf prints that chain as "Apple Issuing Chain".
#
#      `-legacy` because OpenSSL 3 encrypts a p12 with PBES2/PBKDF2/AES-256-CBC
#      by default, and rcodesign's PKCS#12 parser (the p12 crate, 0.6.3 in
#      apple-codesign 0.29.0) decrypts exactly two ciphers, the pre-OpenSSL-3
#      pbeWithSHA1And40BitRC2-CBC and pbeWithSHAAnd3-KeyTripleDES-CBC -- and a
#      bag it cannot decrypt comes out as "incorrect password given when
#      decrypting PFX data", the same words it uses for a wrong password.
#      Measured: one cert and key exported without `-legacy` fail with that
#      message under the right password; exported with it, they sign. The
#      signing stage below asks openssl about the file before rcodesign sees
#      it and names this failure by name. The x509 line first because the
#      .cer the portal downloads is DER, and `pkcs12 -export` reads PEM. A p12
#      written by `rcodesign generate-self-signed-certificate --p12-file` is
#      already in the readable form (measured: its bags report those two
#      ciphers), but a self-signed certificate is no use to the App Store.
#
#   3. Same page -> Identifiers -> "+" -> App IDs -> App, with the bundle ID
#      `app.squallar` (it must match PRODUCT_BUNDLE_IDENTIFIER in
#      packaging/ios/project.yml, which is what the build stamps into the
#      bundle and what the uploader looks the app up by).
#
#   4. Same page -> Profiles -> "+" -> Distribution -> **App Store Connect**.
#      Pick the App ID from step 3 and the certificate from step 2, download
#      the .mobileprovision:
#
#        base64 -w0 squallar.mobileprovision -> IOS_PROVISIONING_PROFILE
#
#      An App Store profile provisions no devices. If the file you downloaded
#      lists `ProvisionedDevices` it is a Development or Ad Hoc profile, and
#      this Makefile will refuse it by name rather than let Apple reject the
#      upload twenty minutes later.
#
#   5. appstoreconnect.apple.com -> Apps -> "+" -> New App, bundle ID
#      `app.squallar`. The upload API attaches a build to an app record that
#      already exists; it does not create one.
#
#   6. appstoreconnect.apple.com -> Users and Access -> Integrations ->
#      App Store Connect API -> Team Keys -> "+", role **App Manager**: the
#      Developer role notarizes but cannot upload builds, and App Manager does
#      both, which is what lets this one key serve the macOS notarization too.
#      Copy the issuer ID and key ID, download the .p8 -- Apple offers it
#      exactly once -- and set the three APPSTORE_CONNECT_* secrets. If the
#      macOS lane already set them, there is nothing to do here. Any
#      APPSTORE_* or MACOS_NOTARY_* secrets still in the repository are read by
#      nothing and should be deleted.
#
#   7. Bump `CURRENT_PROJECT_VERSION` in packaging/ios/project.yml for every
#      upload. App Store Connect rejects a build whose CFBundleVersion it has
#      already seen for that CFBundleShortVersionString.
#
# Until all of that exists the build still runs: it assembles, it says in the
# log that it is not signing and why, and it uploads an artifact NAMED
# `ios-aarch64-UNSIGNED`. A fork pull request, which never gets secrets, takes
# exactly that path. Nothing here fails for want of a paid account.
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
# "built with an outdated SDK" rejection -- before TestFlight, this pipeline
# had no consumer that could tell.
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
IOS_MIN ?= 15.0
# Reads LC_BUILD_VERSION back out of the linked binary. cctools' own `otool`
# and `vtool` are not in this image; llvm-objdump is, and its --macho reader
# prints the same fields. Pinned by absolute path because it is not on PATH.
OBJDUMP ?= /opt/llvm-mingw/bin/llvm-objdump
# Separate from target/, which holds host artifacts: one directory shared by two
# toolchains with two file ownerships is how a stale-lock stall starts.
TARGET_DIR ?= target-ios
# `rcodesign sign`. Not in the rust-cross image; CI installs it (see
# .github/workflows/build.yaml) and a developer gets it with
# `cargo install apple-codesign` or from
# https://github.com/indygreg/apple-platform-rs/releases.
RCODESIGN ?= rcodesign
# Local overrides for the two signing inputs CI passes as base64 secrets. A
# path is what a person has on their own disk; base64 is what fits in a GitHub
# secret. Both spellings reach the same code.
P12_FILE ?=
P12_PASSWORD ?=
PROFILE_FILE ?=

.PHONY: build clean
build:
	@cd "$(ROOT)"
	# One scratch directory, one trap. Everything secret this recipe writes to
	# disk -- the decoded .p12, its password, the provisioning profile -- goes
	# here and is removed on exit, success or failure. A second `trap ... EXIT`
	# later in the recipe would silently replace this one, so there is exactly
	# one.
	SCRATCH="$$(mktemp -d)"
	trap 'rm -rf "$$SCRATCH"' EXIT
	chmod 700 "$$SCRATCH"
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
	    # The SDK's OWN build, which is what DTSDKBuild and DTPlatformBuild
	    # have to carry. It is not in SDKSettings.json; it is the
	    # ProductBuildVersion of the SystemVersion.plist the SDK ships.
	    sed -n '/<key>ProductBuildVersion<\/key>/{n;s|.*<string>\(.*\)</string>.*|\1|p;}' \
	        $$SDKROOT_REF/System/Library/CoreServices/SystemVersion.plist \
	        > $$LIBDIR/sdk-build
	    sed -n '/<key>ProductVersion<\/key>/{n;s|.*<string>\(.*\)</string>.*|\1|p;}' \
	        $$SDKROOT_REF/System/Library/CoreServices/SystemVersion.plist \
	        > $$LIBDIR/sdk-product-version
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

	# ---- which Xcode shipped this SDK ----
	# App Store Connect refuses a build whose Info.plist has no DTXcode /
	# DTXcodeBuild / DTSDKBuild: with them missing it concludes the app was
	# built with a beta toolchain and rejects the upload as error 90534,
	# "Unsupported SDK or Xcode version". Nothing here is Xcode, so nothing
	# writes them, and every iOS upload was refused for it.
	#
	# DTSDKBuild and DTPlatformBuild come off the SDK, so they cannot drift.
	# The Xcode identity cannot: an SDK does not record the Xcode it shipped
	# inside, and this image carries no Xcode at all. So it is a table, keyed
	# on the SDK build -- and an SDK build with no row is a hard error rather
	# than a default, because the failure mode of guessing is a bundle that
	# claims an Xcode which never shipped this SDK, which is a worse lie than
	# the silence it replaces and is checkable by Apple.
	SDK_BUILD="$$(cat "$$LIBDIR/sdk-build")"
	SDK_PRODUCT_VERSION="$$(cat "$$LIBDIR/sdk-product-version")"
	# No apostrophe in this message, and that is not style. Inside
	# $${VAR:?word} bash parses `word` with quote handling, so a lone `'"'"'` opens
	# a single-quoted string that never closes -- the recipe then dies far
	# below with "syntax error near unexpected token }" pointing at an
	# unrelated function three hundred lines away.
	: "$${SDK_BUILD:?the linked SDK has no ProductBuildVersion in its SystemVersion.plist}"
	case "$$SDK_BUILD" in
	  # iOS 26.4 SDK, shipped by Xcode 26.4 (17E192) on 2026-03-24, whose
	  # host macOS build is 25E236. DTXcode is Xcode's version with the dot
	  # removed and a trailing zero, which is Apple's own spelling: 26.4 ->
	  # 2640.
	  23E237) XCODE_BUILD=17E192; DTXCODE=2640; MACHINE_OS_BUILD=25E236 ;;
	  *)
	    echo "ERROR: the image's iPhoneOS SDK has build $$SDK_BUILD, and this"
	    echo "       Makefile has no row for it, so it cannot say which Xcode"
	    echo "       shipped it. DTXcode/DTXcodeBuild would have to be invented,"
	    echo "       and App Store Connect checks them against the SDK."
	    echo ""
	    echo "       Add a row above with the Xcode release that ships SDK build"
	    echo "       $$SDK_BUILD (xcodereleases.com lists the pairing), or pin the"
	    echo "       container image back to one whose SDK is already listed."
	    exit 1 ;;
	esac
	echo "    toolchain: DTSDKBuild=$$SDK_BUILD DTXcode=$$DTXCODE DTXcodeBuild=$$XCODE_BUILD"

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
	    -e "s|\$$(SDK_BUILD)|$$SDK_BUILD|g" \
	    -e "s|\$$(SDK_PRODUCT_VERSION)|$$SDK_PRODUCT_VERSION|g" \
	    -e "s|\$$(DTXCODE)|$$DTXCODE|g" \
	    -e "s|\$$(XCODE_BUILD)|$$XCODE_BUILD|g" \
	    -e "s|\$$(MACHINE_OS_BUILD)|$$MACHINE_OS_BUILD|g" \
	    packaging/ios/Info.plist > "$$APP/Info.plist"
	# An unexpanded $(...) here means a key silently ships as a literal.
	! grep -q '\$$(' "$$APP/Info.plist" || { echo "ERROR: unexpanded build setting left in Info.plist"; exit 1; }
	# Apple's privacy manifest, at the BUNDLE ROOT for iOS -- macOS puts the
	# same file under Contents/Resources instead. Copied verbatim: unlike
	# Info.plist it carries no build settings to expand. Absent, App Store
	# Connect rejects the upload with ITMS-91053 (Missing API declaration).
	# One source for both bundles; see the file's own header.
	# The app icon, rendered from packaging/icon/squallar.svg into an asset
	# catalog. No raster of the icon is committed; see squallar-icon.
	#
	# App Store Connect refuses a bundle with no icon three ways at once --
	# 90022 (no 120x120), 90023 (no 152x152) and 90713 (no CFBundleIconName) --
	# and the third of those is why a catalog rather than loose PNGs: an app
	# built against the iOS 11 or later SDK has to name one.
	cargo run -q -p squallar-icon -- --ios "$$APP"
	test -s "$$APP/AppIcon60x60@2x.png" \
	    || { echo "ERROR: the icon renderer wrote no AppIcon60x60@2x.png, which is the 120x120 error 90022 asks for."; exit 1; }
	test -s "$$APP/AppIcon76x76@2x~ipad.png" \
	    || { echo "ERROR: the icon renderer wrote no AppIcon76x76@2x~ipad.png, which is the 152x152 error 90023 asks for."; exit 1; }
	cp packaging/PrivacyInfo.xcprivacy "$$APP/PrivacyInfo.xcprivacy"
	test -s "$$APP/PrivacyInfo.xcprivacy" || { echo "ERROR: PrivacyInfo.xcprivacy did not reach the bundle root"; exit 1; }
	# iPhone presentation clamps to 60 Hz on ProMotion panels without this key;
	# iPads are exempt.
	grep -A1 '<key>CADisableMinimumFrameDurationOnPhone</key>' "$$APP/Info.plist" | grep -q '<true/>' \
	    || { echo "ERROR: CADisableMinimumFrameDurationOnPhone is not true in the bundled Info.plist"; exit 1; }
	# ---- sign for App Store distribution ----
	# An .ipa App Store Connect will accept needs three things this stage adds:
	# a signature from an Apple Distribution certificate, the App Store
	# provisioning profile as `embedded.mobileprovision` inside the bundle, and
	# entitlements whose `application-identifier` matches both. Nothing here is
	# Apple software -- `rcodesign` is a Rust program and the profile is parsed
	# with openssl and sed.
	#
	# The entitlements are DERIVED FROM THE PROFILE rather than written down
	# here. A hand-written team id that disagrees with the profile is the most
	# common way this fails, and it fails on Apple's side, minutes after the
	# upload, with a message about a mismatched application-identifier. Reading
	# both values out of the one file that is also embedded in the bundle
	# removes the disagreement rather than checking for it.
	#
	# The simulator arm is skipped: the simulator does not check signatures and
	# there is no such thing as an App Store build for it.
	SIGNING=unsigned
	IPA_NAME=squallar-UNSIGNED.ipa
	if [ "$$SDK" != iphonesimulator ]; then
	  PROFILE=""
	  if [ -n "$(PROFILE_FILE)" ]; then
	    PROFILE="$(PROFILE_FILE)"
	  elif [ -n "$${IOS_PROVISIONING_PROFILE:-}" ]; then
	    PROFILE="$$SCRATCH/embedded.mobileprovision"
	    printf '%s' "$${IOS_PROVISIONING_PROFILE}" | base64 -d > "$$PROFILE"
	  fi
	  P12=""
	  if [ -n "$(P12_FILE)" ]; then
	    P12="$(P12_FILE)"
	  elif [ -n "$${IOS_SIGNING_P12:-}" ]; then
	    P12="$$SCRATCH/signing.p12"
	    printf '%s' "$${IOS_SIGNING_P12}" | base64 -d > "$$P12"
	  fi

	  if ! command -v $(RCODESIGN) >/dev/null 2>&1; then
	    SIGNING=no-signer
	  elif [ -z "$$PROFILE" ] || [ -z "$$P12" ]; then
	    SIGNING=no-credentials
	  else
	    echo "==> signing for App Store distribution"
	    # A .mobileprovision is a CMS envelope around a plain XML plist. The
	    # signature is Apple's and there is nothing here that could check it,
	    # so `-noverify` unwraps rather than validates; the fallback reads the
	    # plist straight out of the file, which is what makes a synthetic
	    # profile usable by the rehearsal gate in CI.
	    PPLIST="$$SCRATCH/profile.plist"
	    if openssl smime -inform DER -verify -noverify -in "$$PROFILE" -out "$$PPLIST" 2>/dev/null; then
	      echo "    profile: unwrapped from its CMS envelope"
	    else
	      LC_ALL=C sed -n '/<?xml/,/<\/plist>/p' "$$PROFILE" > "$$PPLIST"
	      echo "    profile: not a CMS envelope; read the plist directly"
	    fi
	    [ -s "$$PPLIST" ] || { echo "ERROR: no plist could be read out of the provisioning profile."; exit 1; }
	    pval() { sed -n "/<key>$$1<\/key>/{n;s/.*<string>\(.*\)<\/string>.*/\1/p;}" "$$PPLIST" | head -1; }
	    APPID="$$(pval application-identifier)"
	    TEAMID="$$(pval com.apple.developer.team-identifier)"
	    [ -n "$$APPID" ]  || { echo "ERROR: the profile declares no application-identifier entitlement."; exit 1; }
	    [ -n "$$TEAMID" ] || { echo "ERROR: the profile declares no com.apple.developer.team-identifier entitlement."; exit 1; }

	    # An App Store profile provisions no specific devices. A Development or
	    # Ad Hoc one lists them, and is the profile a person reaches for by
	    # habit -- it produces a build that installs on their own phone and is
	    # rejected on upload, which is a slow way to find out.
	    if grep -q '<key>ProvisionedDevices</key>' "$$PPLIST"; then
	      echo "ERROR: this provisioning profile lists ProvisionedDevices, so it is a"
	      echo "       Development or Ad Hoc profile, not an App Store one. App Store"
	      echo "       Connect rejects a build signed with it. Create an App Store"
	      echo "       distribution profile; see the note at the top of this file."
	      exit 1
	    fi

	    # A wildcard profile (TEAMID.*) is legal and covers this bundle id; the
	    # entitlement written into the binary must still name the app exactly.
	    case "$$APPID" in
	      *.\*) ENT_APPID="$${APPID%.*}.$$BUNDLE_ID" ;;
	      *)    ENT_APPID="$$APPID" ;;
	    esac
	    if [ "$$ENT_APPID" != "$$TEAMID.$$BUNDLE_ID" ]; then
	      echo "ERROR: the profile is for $$ENT_APPID but this bundle is $$BUNDLE_ID"
	      echo "       under team $$TEAMID. The signature would claim an identity the"
	      echo "       profile does not grant, and Apple would refuse the upload."
	      exit 1
	    fi

	    cp "$$PROFILE" "$$APP/embedded.mobileprovision"

	    # get-task-allow FALSE is the one entitlement that is not a preference.
	    # True means the process is debuggable; App Store Connect rejects any
	    # build carrying it, and it is the default a development profile
	    # supplies, so it is written explicitly rather than left out.
	    ENT="$$SCRATCH/entitlements.plist"
	    {
	      echo '<?xml version="1.0" encoding="UTF-8"?>'
	      echo '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">'
	      echo '<plist version="1.0">'
	      echo '<dict>'
	      echo "	<key>application-identifier</key>"
	      echo "	<string>$$ENT_APPID</string>"
	      echo "	<key>com.apple.developer.team-identifier</key>"
	      echo "	<string>$$TEAMID</string>"
	      echo "	<key>get-task-allow</key>"
	      echo "	<false/>"
	      echo '</dict>'
	      echo '</plist>'
	    } > "$$ENT"

	    PWFILE="$$SCRATCH/p12-password"
	    printf '%s' "$${IOS_SIGNING_P12_PASSWORD:-$(P12_PASSWORD)}" > "$$PWFILE"

	    # ---- can rcodesign read this p12 at all? ----
	    # rcodesign's PKCS#12 parser (the p12 crate, 0.6.3 in apple-codesign
	    # 0.29.0) decrypts exactly two ciphers -- pbeWithSHA1And40BitRC2-CBC and
	    # pbeWithSHAAnd3-KeyTripleDES-CBC, the pre-OpenSSL-3 form -- and a bag in
	    # any other cipher decrypts to the same None as a bag under the wrong
	    # password. Both surface as "incorrect password given when decrypting PFX
	    # data", so a p12 written by OpenSSL 3's default `pkcs12 -export`
	    # (PBES2/PBKDF2/AES-256-CBC) sends the reader chasing a password that is
	    # right. openssl can tell the two apart, so it is asked first when it is
	    # on PATH: the MAC check, which is what a wrong password fails, runs
	    # before the bag ciphers are printed, and the ciphers are printed before
	    # any bag is decrypted. The rust-cross image has it at /usr/bin (3.5.6,
	    # measured), and the ubuntu-24.04 runner has it too -- the ios-testflight
	    # job asserts exactly that.
	    if command -v openssl >/dev/null 2>&1; then
	      P12INFO="$$SCRATCH/p12-info"
	      # `-legacy` first, plain second, and the exit status of neither is what
	      # is read. On OpenSSL 3 the RC2-40 cipher lives in the legacy provider,
	      # which is not loaded by default, so reading a GOOD file without the
	      # flag stops after the first algorithm line with "unsupported ...
	      # RC2-40-CBC" (measured on 3.5.6 and 3.6.3); the retry is for an
	      # openssl that has no such flag. What is inspected is the text: a
	      # "Mac verify error" is the password, a PBES2/PBKDF2/AES line is the
	      # cipher, and neither is a file rcodesign can read.
	      openssl pkcs12 -info -noout -legacy -in "$$P12" -passin "file:$$PWFILE" > "$$P12INFO" 2>&1 \
	        || openssl pkcs12 -info -noout -in "$$P12" -passin "file:$$PWFILE" > "$$P12INFO" 2>&1 \
	        || true
	      if grep -q 'Mac verify error' "$$P12INFO"; then
	        echo ""
	        echo "ERROR: the p12 password is wrong. openssl could not verify the file's MAC"
	        echo "       with the password in IOS_SIGNING_P12_PASSWORD / P12_PASSWORD=, and"
	        echo "       the MAC is checked before any cipher question arises -- so this is"
	        echo "       the password, not the file's encryption."
	        exit 1
	      elif grep -qE 'PBES2|PBKDF2|AES' "$$P12INFO"; then
	        echo ""
	        echo "ERROR: the p12 is encrypted with a cipher rcodesign cannot read:"
	        grep -E '^(PKCS7 Encrypted data|Shrouded Keybag):' "$$P12INFO" | sed 's/^/         /'
	        echo "       The password is right: openssl verified the MAC with it. OpenSSL 3"
	        echo "       writes PBES2/PBKDF2/AES-256 by default; rcodesign's parser reads only"
	        echo "       the legacy RC2-40/3DES/SHA-1 form, and reports this file as"
	        echo "       'incorrect password given when decrypting PFX data'. Re-export it:"
	        echo "         openssl pkcs12 -export -legacy -in cert.pem -inkey key.pem -out squallar-ios.p12"
	        echo "       or from this file alone -- through a file, not a pipe: -export opens"
	        echo "       its input once for the certificate and again for the key, and piped"
	        echo "       it finds no certificate (measured):"
	        echo "         openssl pkcs12 -in old.p12 -out both.pem   # the key stays encrypted"
	        echo "         openssl pkcs12 -export -legacy -in both.pem -out squallar-ios.p12"
	        echo "         rm both.pem"
	        echo "       then replace IOS_SIGNING_P12 with 'base64 -w0' of the result."
	        exit 1
	      else
	        CIPHERS="$$(grep -E '^(PKCS7 Encrypted data|Shrouded Keybag):' "$$P12INFO" | sed 's/^[^:]*: *//; s/,.*//' | sort -u | paste -sd/ || true)"
	        echo "    p12 cipher: $${CIPHERS:-not reported by openssl; rcodesign decides}"
	      fi
	    else
	      echo "    note: no openssl on PATH, so the p12 cipher was not checked before rcodesign."
	    fi

	    # ---- is Certificate 0 the leaf, and the right kind of leaf? ----
	    # rcodesign signs with the FIRST certificate in the p12, and `openssl
	    # pkcs12 -export -certfile` writes the CA it was handed before the leaf.
	    # Measured with 0.29.0: a throwaway CA with OU=G2 -- the OU Apple's
	    # Developer ID CA G2 carries -- in front of a leaf with OU=ABCDE12345
	    # produced a signature whose certificate was the CA and whose team id
	    # was G2; the same leaf and key alone gave ABCDE12345. Under
	    # --for-notarization both throwaways fail with the same two lines ("not
	    # signed by Apple", "doesn't appear to be such a certificate"), so
	    # rcodesign's own words name the certificate's TYPE when the cause is
	    # its ORDER. The Team ID tells them apart: Apple's are ten upper-case
	    # alphanumerics and a CA's OU is not. Nothing but the leaf and its key
	    # belongs in the file: rcodesign carries Apple's CA certificates itself
	    # (src/apple-certs/DeveloperIDG2CA.cer among them) and adds the chain
	    # from the leaf upward when it loads the key (chain_apple_certificates,
	    # called from cli/certificate_source.rs); `analyze-certificate` on a bare
	    # Apple-issued leaf prints that resolved chain as "Apple Issuing Chain",
	    # measured on the leaves the crate ships for its own tests.
	    CERTINFO="$$SCRATCH/certificate-0"
	    $(RCODESIGN) analyze-certificate --p12-file "$$P12" --p12-password-file "$$PWFILE" 2>/dev/null \
	      | awk '/^# Certificate 0$$/{f=1; next} /^# Certificate [0-9]/{f=0} /^-----BEGIN/{f=0} f' > "$$CERTINFO" || true
	    [ -s "$$CERTINFO" ] || { echo "ERROR: $(RCODESIGN) analyze-certificate printed no 'Certificate 0' for the p12."; exit 1; }
	    cfield() { sed -n "s/^$$1: *//p" "$$CERTINFO" | head -1; }
	    C_SUBJECT="$$(cfield 'Subject CN')";      C_ISSUER="$$(cfield 'Issuer CN')"
	    C_TEAM="$$(cfield 'Team ID')";            C_SELF="$$(cfield 'Subject is Issuer?')"
	    C_APPLE="$$(cfield 'Signed by Apple?')";  C_PROFILE="$$(cfield 'Guessed Certificate Profile')"
	    C_INTER="$$(cfield 'Is Apple Intermediate CA?')"; C_ROOT="$$(cfield 'Is Apple Root CA?')"
	    echo "    certificate 0: $$C_SUBJECT (team id $$C_TEAM, profile $$C_PROFILE, Apple-issued: $$C_APPLE)"
	    C_LEAF=; printf '%s' "$$C_TEAM" | LC_ALL=C grep -qxE '[A-Z0-9]{10}' && C_LEAF=1
	    # A CA in slot 0 is wrong on every path, and it looks like this: Apple's own
	    # intermediates rcodesign recognises by name, anything else's carries an OU
	    # that is not a team id. A self-signed certificate with NO team id (the
	    # throwaway this file's header makes prints 'unset'; one with no OU at all
	    # prints '<missing>') is not this case; it is handled on the Apple-facing
	    # path below and is a legitimate identity on the other one.
	    if [ "$$C_INTER" = true ] || [ "$$C_ROOT" = true ] \
	       || { [ -z "$$C_LEAF" ] && [ "$$C_TEAM" != "<missing>" ] && [ "$$C_TEAM" != unset ]; }; then
	      echo ""
	      echo "ERROR: Certificate 0 in the p12 is not your leaf -- a Team ID of '$$C_TEAM' is a"
	      echo "       CA's OU; rebuild the p12 with -inkey + -in <leaf> only, no -certfile."
	      echo "       rcodesign signs with the first certificate in the file, and OpenSSL"
	      echo "       writes a -certfile CA first. Apple's intermediates do not belong in"
	      echo "       the file: rcodesign carries them and chains them at sign time."
	      echo "         openssl pkcs12 -export -legacy -inkey <key.pem> -in <leaf.pem> -out squallar-ios.p12"
	      echo "       then replace IOS_SIGNING_P12 with 'base64 -w0' of the result."
	      exit 1
	    fi
	    # This arm IS the App Store path, so the check is unconditional, and
	    # the three ways to hold the wrong kind of certificate are three
	    # different errands, so each is named. App Store Connect would refuse
	    # all three after the upload; this is what makes them different
	    # messages, before it.
	    if [ "$$C_SELF" = true ]; then
	      echo ""
	      echo "ERROR: Certificate 0 is self-signed ($$C_SUBJECT). App Store Connect accepts"
	      echo "       only an Apple Distribution certificate Apple issued; a self-signed"
	      echo "       one signs a bundle nothing can install. Request one (steps 1-2 at"
	      echo "       the top) and replace IOS_SIGNING_P12, or clear IOS_SIGNING_P12 and"
	      echo "       IOS_PROVISIONING_PROFILE to get the UNSIGNED build back."
	      exit 1
	    elif [ "$$C_APPLE" != true ]; then
	      echo ""
	      echo "ERROR: Certificate 0 is a leaf for team $$C_TEAM, but it was not issued by Apple"
	      echo "       (issuer: $$C_ISSUER), and App Store Connect takes only what Apple's own"
	      echo "       CA signed. An Apple Distribution certificate comes from"
	      echo "       developer.apple.com against a CSR: steps 1-2 at the top of this file."
	      exit 1
	    elif [ "$$C_PROFILE" != AppleDistribution ]; then
	      echo ""
	      echo "ERROR: Certificate 0 is an Apple-issued '$$C_PROFILE' certificate, not an"
	      echo "       Apple Distribution one. Apple Development signs builds for machines"
	      echo "       on the team and App Store Connect rejects them; Developer ID"
	      echo "       Application is the macOS lane's and the App Store will not take it."
	      echo "       Request Apple Distribution (step 2 at the top) and rebuild the p12."
	      exit 1
	    fi

	    $(RCODESIGN) sign --p12-file "$$P12" --p12-password-file "$$PWFILE" \
	        --entitlements-xml-path "$$ENT" "$$APP"
	    SIGNING=distribution
	    IPA_NAME=squallar.ipa
	    echo "    signed $$ENT_APPID (team $$TEAMID)"
	  fi

	  # ---- the guards the signing stage exists for ----
	  # Read out of the signature that was written, not out of the arguments we
	  # believe we passed. rcodesign emits the entitlements TWICE -- an XML
	  # copy and a DER copy -- and Apple reads the DER one, so a check that
	  # looked at the first match would pass while the field that matters said
	  # something else. Both are counted.
	  if [ "$$SIGNING" = distribution ]; then
	    SIGINFO="$$SCRATCH/signature-info.yaml"
	    $(RCODESIGN) print-signature-info "$$APP" > "$$SIGINFO" 2>/dev/null
	    NKEY="$$(grep -c '<key>get-task-allow</key>' "$$SIGINFO" || true)"
	    NFALSE="$$(grep -A1 '<key>get-task-allow</key>' "$$SIGINFO" | grep -c '<false' || true)"
	    NTRUE="$$(grep -A1 '<key>get-task-allow</key>' "$$SIGINFO" | grep -c '<true' || true)"
	    if [ "$$NKEY" != 2 ]; then
	      echo "ERROR: the signature carries $$NKEY get-task-allow entitlements; expected"
	      echo "       2 (the XML copy and the DER copy rcodesign writes). Either"
	      echo "       signing did not apply the entitlements or print-signature-info"
	      echo "       changed shape, and in both cases this guard has stopped"
	      echo "       checking anything."
	      exit 1
	    fi
	    if [ "$$NTRUE" != 0 ] || [ "$$NFALSE" != 2 ]; then
	      echo "ERROR: get-task-allow is not false in both copies of the entitlements"
	      echo "       ($$NTRUE true, $$NFALSE false). A true value means the app is"
	      echo "       debuggable and is an automatic App Store Connect rejection."
	      exit 1
	    fi
	    for k in application-identifier com.apple.developer.team-identifier; do
	      n="$$(grep -c "<key>$$k</key>" "$$SIGINFO" || true)"
	      [ "$$n" = 2 ] || { echo "ERROR: $$k appears $$n times in the signature, expected 2."; exit 1; }
	    done
	    [ -f "$$APP/embedded.mobileprovision" ] \
	        || { echo "ERROR: no embedded.mobileprovision in the signed bundle."; exit 1; }
	    echo "    entitlements: get-task-allow false in both copies, profile embedded"
	  fi
	fi
	printf '%s\n' "$$SIGNING" > "$$OUT/SIGNING"

	echo ""
	if [ "$$SDK" = iphonesimulator ]; then
	  echo "App: $(ROOT)/$$APP  (simulator, arm64)"
	  echo "Run on an Apple Silicon macOS host:"
	  echo "  xcrun simctl boot 'iPhone 15' || true"
	  echo "  xcrun simctl install booted '$$APP'"
	  echo "  xcrun simctl launch --console booted $$BUNDLE_ID"
	elif command -v zip >/dev/null 2>&1; then
	  # The NAME is the label, exactly as in packaging/macos/Makefile.
	  # `squallar.ipa` means a distribution signature and an embedded App Store
	  # profile -- something that can be uploaded. Every other outcome is
	  # `squallar-UNSIGNED.ipa`, which installs nowhere and uploads nowhere, so
	  # a fork pull request's artifact cannot be mistaken for a shippable one a
	  # week later when only the artifact list is left.
	  rm -rf "$$OUT/Payload"; mkdir -p "$$OUT/Payload"
	  cp -R "$$APP" "$$OUT/Payload/"
	  ( cd "$$OUT" && zip -qr "$$IPA_NAME" Payload && rm -rf Payload )
	  case "$$SIGNING" in
	    distribution)
	      echo "App: $(ROOT)/$$APP  (signed for App Store distribution)"
	      echo "IPA: $(ROOT)/$$OUT/$$IPA_NAME  (uploadable)"
	      echo "Upload it with:"
	      echo "  .github/scripts/appstore-upload.py --ipa $$OUT/$$IPA_NAME" ;;
	    no-signer)
	      echo "App: $(ROOT)/$$APP  (UNSIGNED)"
	      echo "IPA: $(ROOT)/$$OUT/$$IPA_NAME  (UNSIGNED)"
	      echo "note: $(RCODESIGN) is not on PATH, so nothing was signed. Install it with"
	      echo "      'cargo install apple-codesign', or from"
	      echo "      https://github.com/indygreg/apple-platform-rs/releases" ;;
	    *)
	      echo "App: $(ROOT)/$$APP  (UNSIGNED)"
	      echo "IPA: $(ROOT)/$$OUT/$$IPA_NAME  (UNSIGNED)"
	      echo "note: no IOS_SIGNING_P12 / IOS_PROVISIONING_PROFILE given, so this"
	      echo "      .ipa will not install on a device and cannot be uploaded. See"
	      echo "      'What a person has to do' at the top of this file." ;;
	  esac
	else
	  # The rust-cross image has no zip. The .app is the artifact that matters;
	  # the .ipa is only Payload/ in a zip, so skipping it loses no coverage.
	  echo "App: $(ROOT)/$$APP  ($$SIGNING)"
	  echo "note: no 'zip' on PATH, so no .ipa was packaged."
	fi

clean:
	@rm -rf "$(ROOT)/packaging/ios/build" "$(ROOT)/$(TARGET_DIR)"
