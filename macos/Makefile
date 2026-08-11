# macOS app build, run entirely on Linux inside the rust-cross container:
#
#   make -C macos                    arm64  (aarch64-apple-darwin), release
#   make -C macos ARCH=x86_64        Intel  (x86_64-apple-darwin),  release
#   make -C macos DEBUG=1            debug profile
#   make -C macos BIN=path/to/exe    package a binary that already exists
#   make -C macos ARCHIVE=1          also produce rustdar.app.zip
#
# `ARCHIVE` and not the obvious `ZIP`: a variable set on make's command line is
# exported into the recipe's environment, and Info-ZIP's `zip` reads `ZIP` (and
# `ZIPOPT`) as a list of default options to prepend to its own argv. `ZIP=1`
# therefore turns `zip -qry rustdar.app.zip rustdar.app` into
# `zip 1 -qry rustdar.app.zip rustdar.app`, which succeeds, and writes the
# archive to `1.zip`. Found by running it.
#
# No Apple tools and no macOS host are involved: the .app is assembled with
# cp/sed, and signed with `rcodesign`, which is a Rust program.
#
# CI already runs *inside* the image, so it sets IN_CONTAINER=1 and the recipe
# executes directly instead of nesting a second container.
#
# This mirrors ios/Makefile, with three differences that are not stylistic:
#
#   * The layout is not flat. A macOS bundle is
#     Contents/{Info.plist, MacOS/rustdar, Resources/, PkgInfo}; an iOS one puts
#     all of that at the top level.
#   * There is no ObjC shell. winit's AppKit backend brings up NSApplication
#     itself, so `rustdar-platform`'s own `main` is the executable and there is
#     nothing to compile or link by hand.
#   * Signing is not optional. arm64 macOS refuses to execute an unsigned
#     Mach-O at all, and — the reason this file exists — `locationd` keys its
#     permission grant to the code signature, so an app whose identity changes
#     every build is an app that re-prompts every build.
#
# ── The signing identity ────────────────────────────────────────────────
#
# macOS remembers "this app may use your location" against the app's
# *designated requirement*. Signed with a certificate, that requirement pins
# the signing leaf, so every build signed with the same certificate is the same
# application and the grant survives. Signed ad-hoc, it pins the code hash
# instead, so every rebuild is a new application and the user is asked again.
#
# Make one certificate, once, and keep using it:
#
#   rcodesign generate-self-signed-certificate \
#       --person-name rustdar --validity-days 3650 \
#       --p12-file rustdar-signing.p12 --p12-password "$$PW"
#
# Then either point this Makefile at it —
#
#   make -C macos P12_FILE=../rustdar-signing.p12 P12_PASSWORD="$$PW"
#
# — or, for CI, put `base64 -w0 rustdar-signing.p12` in the repository secret
# `MACOS_SIGNING_P12` and the password in `MACOS_SIGNING_P12_PASSWORD`; this
# recipe reads both from the environment. Those secrets do not exist until
# somebody creates them, and no workflow can create them.
#
# `--validity-days 3650`, not the 365 default: an expiry forces a new
# certificate, and a new certificate costs every user one more prompt.
#
# With no certificate at all this falls back to an ad-hoc signature, which is
# a real signature (so the app launches) with an unstable identity (so it
# re-prompts). That is the right fallback for a fork pull request, which never
# has access to the secret — but it is labelled in the log, not silent.
SHELL := bash
.SHELLFLAGS := -euo pipefail -c
.ONESHELL:
ROOT := $(realpath $(CURDIR)/..)

# Digest-pinned to the same image the CI matrix uses.
IMAGE ?= ghcr.io/usa-reddragon/rust-cross:1.97.1@sha256:433d3e61c46c6dea2e0bb0bc450fe34415d9769f6d801ab1f2e2ebbdbcf50c28
# Named volume for the crate cache, so a rebuild does not re-download the index.
CARGO_VOL ?= rustdar-xcross-cargo
# arm64 by default: it is what every Mac sold since 2020 is, and the one arch
# where an unsigned build will not even launch.
ARCH ?= aarch64
# Separate from target/, which holds host artifacts.
TARGET_DIR ?= target-macos
# `rcodesign sign`. Not in the rust-cross image; CI installs it (see
# .github/workflows/build.yaml) and a developer gets it with
# `cargo install apple-codesign` or from
# https://github.com/indygreg/apple-platform-rs/releases.
RCODESIGN ?= rcodesign

.PHONY: build clean
build:
	@cd "$(ROOT)"
	case "$(ARCH)" in
	  aarch64|arm64) RUST_TARGET=aarch64-apple-darwin ;;
	  x86_64|amd64)  RUST_TARGET=x86_64-apple-darwin ;;
	  *) echo "ERROR: ARCH must be aarch64 or x86_64, not '$(ARCH)'"; exit 1 ;;
	esac
	if [ -n "$(DEBUG)" ]; then PROFILE_DIR=debug; CARGO_PROFILE=; else PROFILE_DIR=release; CARGO_PROFILE=--release; fi

	# ---- the executable ----
	# BIN= names one that already exists. CI passes it because the build matrix
	# has already compiled exactly this binary for exactly this triple, and
	# compiling the workspace a second time to package it would double the
	# row's wall clock for a file that is already on disk.
	#
	# The corollary is that BIN= picks the profile too. DEBUG= and the default
	# `--release` above govern a build this recipe runs itself and nothing
	# else, so a caller handing over a path under debug/ gets a debug .app no
	# matter what else it passed — which is how every published macOS build
	# came to be an unoptimized one. Said out loud here rather than left to be
	# read off a path.
	if [ -n "$(BIN)" ]; then
	  EXE="$(BIN)"
	  [ -f "$$EXE" ] || { echo "ERROR: BIN=$$EXE does not exist"; exit 1; }
	  echo "==> packaging the existing $$EXE"
	  case "$$EXE" in
	    */debug/*)
	      echo "    NOTE: a debug-profile binary. The .app will carry workspace code at"
	      echo "          opt-level = 0 with debug assertions on."
	      ;;
	  esac
	else
	  EXE="$(TARGET_DIR)/$$RUST_TARGET/$$PROFILE_DIR/rustdar-platform"
	  # RUSTUP_TOOLCHAIN from the image's own RUST_VERSION: rust-toolchain.toml
	  # says `stable`, and obeying it here fetches a toolchain that has none of
	  # the image's 23 preinstalled targets.
	  #
	  # MACOSX_DEPLOYMENT_TARGET is deliberately *not* exported here. It lives
	  # in /.cargo/config.toml so that every `cargo` invocation gets it,
	  # including bare `cargo check` rows and a developer's local build; an
	  # export here would cover only this one path and would be the copy that
	  # drifts. The guard further down verifies the linked binary really got it.
	  INCMD="set -e
	      export RUSTUP_TOOLCHAIN=\"\$${RUST_VERSION:?the rust-cross image must set RUST_VERSION}\"
	      export CARGO_TARGET_DIR=$(TARGET_DIR)
	      cargo build $$CARGO_PROFILE --target $$RUST_TARGET -p rustdar-platform --bin rustdar-platform"
	  if [ -n "$(IN_CONTAINER)" ]; then
	    echo "==> cargo build ($$RUST_TARGET, $$PROFILE_DIR)"
	    sh -c "$$INCMD"
	  else
	    ENGINE="$${RUSTDAR_CONTAINER_ENGINE:-}"
	    if [ -z "$$ENGINE" ]; then
	      if command -v podman >/dev/null 2>&1; then ENGINE=podman
	      elif command -v docker >/dev/null 2>&1; then ENGINE=docker
	      else echo "ERROR: need podman or docker (set RUSTDAR_CONTAINER_ENGINE, or IN_CONTAINER=1 if already inside the image)."; exit 1; fi
	    fi
	    echo "==> [$$ENGINE] cargo build ($$RUST_TARGET, $$PROFILE_DIR)"
	    # Rootless podman already maps container root to the invoking user, so
	    # chowning there would push the files onto a subuid instead.
	    if [ "$$ENGINE" = docker ]; then INCMD="$$INCMD
	      chown -R $$(id -u):$$(id -g) $(TARGET_DIR)"; fi
	    "$$ENGINE" run --rm \
	        -v "$(ROOT)":/project -w /project \
	        -v "$(CARGO_VOL)":/usr/local/cargo/registry \
	        "$(IMAGE)" sh -c "$$INCMD"
	  fi
	  [ -f "$$EXE" ] || { echo "ERROR: the build produced no executable at $$EXE"; exit 1; }
	fi

	# ---- bundle metadata ----
	# Single source: macos/project.yml. Its own file and never ios/project.yml:
	# this reader takes the first `KEY:` in the file, comments included, and the
	# iOS spec's comment block names two of these keys before its settings do.
	yaml_val() { sed -n "s/.*$$1: *\"\{0,1\}\([^\"]*\)\"\{0,1\}.*/\1/p" macos/project.yml | head -1; }
	BUNDLE_ID="$$(yaml_val PRODUCT_BUNDLE_IDENTIFIER)"
	MARKETING_VERSION="$$(yaml_val MARKETING_VERSION)"
	CURRENT_PROJECT_VERSION="$$(yaml_val CURRENT_PROJECT_VERSION)"
	MACOS_MIN="$$(yaml_val MACOSX_DEPLOYMENT_TARGET)"
	: "$${BUNDLE_ID:?could not read PRODUCT_BUNDLE_IDENTIFIER from macos/project.yml}"
	: "$${MACOS_MIN:?could not read MACOSX_DEPLOYMENT_TARGET from macos/project.yml}"

	# The plist's floor and the compiler's floor are two different files, and
	# the whole point of the floor is that a binary built below it links
	# cleanly and then `doesNotRecognizeSelector:` on a user's Mac. So they are
	# checked against each other, and then both are checked against what the
	# linker actually wrote.
	CARGO_MIN="$$(sed -n 's/^ *MACOSX_DEPLOYMENT_TARGET *= *"\([^"]*\)".*/\1/p' .cargo/config.toml | head -1)"
	: "$${CARGO_MIN:?no MACOSX_DEPLOYMENT_TARGET in .cargo/config.toml; the Apple deployment floor is unset}"
	[ "$$CARGO_MIN" = "$$MACOS_MIN" ] || {
	  echo "ERROR: .cargo/config.toml says MACOSX_DEPLOYMENT_TARGET=$$CARGO_MIN"
	  echo "       macos/project.yml says MACOSX_DEPLOYMENT_TARGET=$$MACOS_MIN"
	  exit 1
	}

	# ---- assemble the .app (plain file ops, no Apple tools) ----
	echo "==> assemble rustdar.app"
	OUT="macos/build/$$RUST_TARGET"; APP="$$OUT/rustdar.app"
	rm -rf "$$OUT"; mkdir -p "$$APP/Contents/MacOS" "$$APP/Contents/Resources"
	cp "$$EXE" "$$APP/Contents/MacOS/rustdar"; chmod +x "$$APP/Contents/MacOS/rustdar"
	# Four bytes of type and creator. Vestigial, and every real .app has it.
	printf 'APPL????' > "$$APP/Contents/PkgInfo"
	# Resolve the Xcode build-setting variables xcodebuild would have expanded.
	sed -e "s|\$$(EXECUTABLE_NAME)|rustdar|g" \
	    -e "s|\$$(PRODUCT_BUNDLE_IDENTIFIER)|$$BUNDLE_ID|g" \
	    -e "s|\$$(MARKETING_VERSION)|$$MARKETING_VERSION|g" \
	    -e "s|\$$(CURRENT_PROJECT_VERSION)|$$CURRENT_PROJECT_VERSION|g" \
	    -e "s|\$$(MACOSX_DEPLOYMENT_TARGET)|$$MACOS_MIN|g" \
	    macos/Info.plist > "$$APP/Contents/Info.plist"
	# An unexpanded $(...) here means a key silently ships as a literal.
	! grep -q '\$$(' "$$APP/Contents/Info.plist" || { echo "ERROR: unexpanded build setting left in Info.plist"; exit 1; }

	# ---- the guard the deployment floor exists for ----
	# Reads the floor out of the Mach-O rather than out of any of the three
	# files that claim it. Rust does no availability checking at all, so this is
	# the only stage between "wrote 11.0 down somewhere" and "a user's Mac
	# throws doesNotRecognizeSelector:".
	OTOOL="$$(command -v otool 2>/dev/null || true)"
	if [ -z "$$OTOOL" ]; then OTOOL="$$(ls /opt/osxcross/bin/*-apple-darwin*-otool 2>/dev/null | head -1 || true)"; fi
	if [ -n "$$OTOOL" ]; then
	  MINOS="$$("$$OTOOL" -l "$$APP/Contents/MacOS/rustdar" \
	    | grep -A6 -E 'LC_BUILD_VERSION|LC_VERSION_MIN_MACOSX' \
	    | grep -oE '^ *(minos|version) [0-9.]+' | head -1 | awk '{print $$2}' || true)"
	  : "$${MINOS:?could not read a deployment target out of the linked binary}"
	  [ "$$MINOS" = "$$MACOS_MIN" ] || {
	    echo "ERROR: the linked binary claims macOS $$MINOS, the bundle claims $$MACOS_MIN."
	    echo "       MACOSX_DEPLOYMENT_TARGET did not reach the compiler. Check that"
	    echo "       .cargo/config.toml is being discovered (cargo looks upward from the"
	    echo "       working directory), and that nothing in the environment overrode it."
	    exit 1
	  }
	  echo "    binary deployment target: macOS $$MINOS (matches the bundle)"
	  "$$OTOOL" -L "$$APP/Contents/MacOS/rustdar" | grep -q CoreLocation || {
	    echo "ERROR: CoreLocation.framework is not linked into the binary."; exit 1; }
	  echo "    CoreLocation.framework: linked"
	else
	  echo "    note: no otool on PATH, so the binary's deployment target was not verified."
	fi

	# ---- sign ----
	# Read the long note at the top: this is not hardening, it is what stops
	# locationd from treating every build as a different application.
	if ! command -v $(RCODESIGN) >/dev/null 2>&1; then
	  echo ""
	  echo "WARNING: $(RCODESIGN) is not on PATH, so rustdar.app is UNSIGNED."
	  echo "         An arm64 Mac will refuse to execute it at all, and on any Mac"
	  echo "         macOS will treat each rebuild as a new app and re-ask for"
	  echo "         location permission. Install it with:"
	  echo "           cargo install apple-codesign"
	  echo "         or from https://github.com/indygreg/apple-platform-rs/releases"
	elif [ -n "$${MACOS_SIGNING_P12:-}$(P12_FILE)" ]; then
	  # A real, stable identity. The p12 arrives either as a path or, in CI, as
	  # base64 in a secret — GitHub secrets are text.
	  P12="$(P12_FILE)"
	  if [ -z "$$P12" ]; then
	    P12="$$(mktemp)"; trap 'rm -f "'"$$P12"'"' EXIT
	    printf '%s' "$${MACOS_SIGNING_P12}" | base64 -d > "$$P12"
	  fi
	  PWFILE="$$(mktemp)"; printf '%s' "$${MACOS_SIGNING_P12_PASSWORD:-$(P12_PASSWORD)}" > "$$PWFILE"
	  echo "==> signing with the project identity"
	  $(RCODESIGN) sign --p12-file "$$P12" --p12-password-file "$$PWFILE" "$$APP"
	  rm -f "$$PWFILE"
	  echo "    signed. The designated requirement is pinned to this certificate,"
	  echo "    so a rebuild signed with the same one keeps its location grant."
	else
	  # Ad-hoc: a valid signature, so an arm64 Mac will run it — but the
	  # designated requirement is the code directory hash, which changes with
	  # every byte of the binary. locationd will re-prompt on every rebuild.
	  echo "==> signing ad-hoc (no MACOS_SIGNING_P12 / P12_FILE given)"
	  $(RCODESIGN) sign "$$APP"
	  echo "    NOTE: ad-hoc. This app will launch, and macOS will ask for"
	  echo "    location permission again after every rebuild."
	fi

	if [ -n "$(ARCHIVE)" ]; then
	  # actions/upload-artifact drops the executable bit, which turns a .app
	  # into a directory macOS declines to launch. A zip carries the mode.
	  command -v zip >/dev/null 2>&1 || { echo "ERROR: ARCHIVE=1 but no zip on PATH"; exit 1; }
	  ( cd "$$OUT" && rm -f rustdar.app.zip && zip -qry rustdar.app.zip rustdar.app )
	  echo "Zip: $(ROOT)/$$OUT/rustdar.app.zip"
	fi
	echo ""
	echo "App: $(ROOT)/$$APP"
	echo "Run it with:  open '$(ROOT)/$$APP'"

clean:
	@rm -rf "$(ROOT)/macos/build" "$(ROOT)/$(TARGET_DIR)"
