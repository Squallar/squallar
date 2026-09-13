---
name: cross-build
description: Build and check squallar for iOS, macOS, Windows and Android ON THIS LINUX BOX, in the same rust-cross container image CI's Build workflow runs in. Use before landing anything that touches `squallar/` (the shell crate), a `cfg(target_os = ...)` arm, `squallar-location`, packaging/, or a dependency with platform-specific code — and whenever someone says "iOS/macOS can't be built here". They can, and this file is the measurement.
---

# Cross-building on this box

**Every target in CI's Build matrix except MSVC is buildable here, today, from
this Linux box.** The image CI pins — `ghcr.io/usa-reddragon/rust-cross:1.97.1`
at digest `68d608cf…` — carries osxcross (both macOS triples), the iPhoneOS and
iPhoneSimulator SDKs, llvm-mingw (Windows gnullvm), the NDK + cargo-ndk, and
wasm-pack, with every `CARGO_TARGET_*_LINKER` and `CC_*` already exported. It is
**already pulled into podman** (15.9 GB, image id `015f9750cbe1`), and the two
Apple packaging Makefiles already know how to run themselves inside it from the
host. Measured 2026-09-12 on `e3dcc1389`: see the table at the end.

"No Apple SDK on this box" is a statement about the host `rustup`, not about
the box. Do not re-adjudicate it; run the row.

## The one recipe

```sh
IMG=ghcr.io/usa-reddragon/rust-cross@sha256:68d608cfe4232a8a9b42ce41ea26412eaadc8f53218fed7e52e43974b388a302
export CONTAINERS_CONF_OVERRIDE=$HOME/.cache/rd-board-lock/podman-caged.conf   # see "The cage"

~/.cache/rd-land-check/board.sh xcross-<label> -- \
  podman run --rm \
    -v "$PWD":/project -w /project \
    -v squallar-xcross-cargo:/usr/local/cargo/registry \
    "$IMG" bash -c '
      set -o pipefail
      export RUSTUP_TOOLCHAIN="${RUST_VERSION:?the image sets this}"
      export CARGO_TARGET_DIR=/project/target/xcross
      <row from the table below> -j 4'
```

Run it from the **worktree you are gating** (a lane's worktree under
`~/.cache/…`, never the shared checkout), so `/project` is that tree.

Four things in it are load-bearing; each was learnt the hard way:

- **Address the image by digest, never by the `1.97.1` tag.** The tag's local
  copy has broken overlay storage (`podman inspect` dies with
  `faccessat …/overlay/d178f1c3…: no such file or directory` — a layer lost to
  one of the 2026-09-11 hard lockups). The digest-pinned copy is intact and is
  the one CI uses (`build.yaml:75`, both Makefiles' `IMAGE ?=`).
- **`podman`, not `docker`.** Both CLIs are installed; only podman has the
  image. Rootless podman maps container root to your uid, so everything it
  writes under `/project` is owned by you — no `chown`, no `-u`.
- **`RUSTUP_TOOLCHAIN=$RUST_VERSION`.** `rust-toolchain.toml` says `stable`;
  obeyed inside the image that fetches a fresh toolchain with none of the
  image's 23 targets and the row dies at "target may not be installed". CI
  does exactly this (`build.yaml` "Use the image's toolchain").
- **Its own target dir.** `target/xcross/` for checks (CI uses `target/`, but
  locally that is the host toolchain's); the iOS Makefile uses `target-ios/`.
  A subdirectory of `target/` and never `target/` itself: cargo's host build
  owns `target/debug`, `target/release` and `target/<triple>`, and nothing it
  writes is named `xcross`. Under `target/` because `/target` is gitignored and
  a sibling `target-xcross/` is not — a lane that ran the rows left it
  untracked in the tree.

And one thing to never do: **do not set `RUSTFLAGS`.** It replaces every
`[target.*]` block in `.cargo/config.toml` wholesale — the Windows row loses
`+crt-static` and the shipped exe dies at the loader (`.cargo/config.toml`
says why at length). The deployment floors (`MACOSX_DEPLOYMENT_TARGET`,
`IPHONEOS_DEPLOYMENT_TARGET`) live in that file's `[env]` and are picked up
inside the container automatically because the repo is the mount.

## The rows — CI's commands verbatim (`.github/workflows/build.yaml` matrix)

| row | target | command inside the container |
|---|---|---|
| macos-aarch64 | `aarch64-apple-darwin` | `cargo check --workspace --all-targets --features squallar/jni-typecheck --target aarch64-apple-darwin && cargo build --release -p squallar --features squallar/jni-typecheck --target aarch64-apple-darwin` |
| macos-x86_64 | `x86_64-apple-darwin` | same with `x86_64-apple-darwin` |
| windows-x86_64 | `x86_64-pc-windows-gnullvm` | same with `x86_64-pc-windows-gnullvm` (gnullvm, not MSVC — MSVC is the one family the image lacks) |
| ios-aarch64 | `aarch64-apple-ios` | `make -C packaging/ios IN_CONTAINER=1` — a real staticlib **link** (`cargo rustc --crate-type staticlib` + clang against the iPhoneOS SDK), not a check |
| ios-aarch64-sim | `aarch64-apple-ios-sim` | `make -C packaging/ios IN_CONTAINER=1 SIMULATOR=1` |
| android-arm64-v8a / x86_64 | `aarch64-linux-android` / `x86_64-linux-android` | gradle `assembleRelease -PabiFilter=…` (downloads SDK packages; needs network). The gate-shaped equivalent is `cargo ndk -t arm64-v8a -P 28 check -p squallar --lib`, which also works on the **host** (`ANDROID_NDK_HOME=/opt/android-sdk/ndk/27.3.13750724`) |
| linux-x86_64 | host | `cargo check --workspace --all-targets --features squallar/jni-typecheck --target x86_64-unknown-linux-gnu` |

**The cheap gate for a lane** is the `cargo check --workspace --all-targets
--features squallar/jni-typecheck --target <triple>` half of a row, for every
triple the change can reach. The iOS rows have no check half in CI because
their failures are link failures (a missing `-framework`, a dep reaching for
IOKit); `cargo check -p squallar --lib --target aarch64-apple-ios` still catches
every `cfg`-resolution break — it is what turned `26d2e375f` red in 65 s.

### Which rows a change owes

| the diff touches | run |
|---|---|
| `squallar/src/` (shell crate: `run.rs`, `platform.rs`, `lib.rs`, `android/`) | **all** of: both iOS checks, both macOS checks, Windows, `cargo ndk` |
| any `#[cfg(target_os = …)]` / `cfg(mobile)` / `cfg(any(target_os…))` arm, anywhere | the triples that arm names or excludes — *and one it does not*, so a name that only exists on one arm is seen from the other |
| `squallar-location`, `os_location/apple.rs`, anything `objc`/`core-foundation` | both macOS + both iOS |
| `packaging/ios/`, `packaging/macos/` | the full `make` for that platform (below) |
| `Cargo.toml` / `Cargo.lock` dependency changes | every triple — a new dep is where "does not cross-compile to wasm32/iOS" lives (`data.md` names `openjpeg-sys`, `libaec-sys`, `proj-sys`, `libsqlite3-sys` as ones already dropped for it) |
| nothing above | nothing owed; the host board is the board |

Why this matters: **the Clippy workflow dropped its Android row ("HOST ONLY",
`clippy.yaml:119`), so nothing pre-push compiles the shell crate for any mobile
or Apple target.** The Build workflow is post-push. `26d2e375f` landed green on
every local and CI pre-push row and turned four Build jobs red at a symbol
that was `cfg`'d out on android/ios. Fixed in `e3dcc1389`; this file exists so
the next one is caught before the land.

## Full packages: the Makefiles run the container themselves

```sh
make -C packaging/ios SIMULATOR=1        # .app for the simulator (unsigned, no Apple tools)
make -C packaging/ios                    # device .app + squallar-UNSIGNED.ipa
make -C packaging/macos                  # arm64 .app  (ARCH=x86_64 for Intel; ARCHIVE=1 for .zip)
```

Run these from the **host**; each detects `podman`, mounts the tree at
`/project` and the registry volume `squallar-xcross-cargo`, and executes the
same recipe CI runs with `IN_CONTAINER=1`. Signing needs `rcodesign` and the
keys CI holds; a local build is unsigned and says so in the artifact name. Both
Makefiles carry the reasons for every step — read them before changing one.

A full make is a **release build of the whole graph with fat LTO**, so it goes
under the lock and the cage like any other board. The host-side `make` does
not forward a `-j`, and the box rule is `-j 4`; the spelling that keeps both is
CI's row verbatim inside a container you start yourself:

```sh
~/.cache/rd-land-check/board.sh xcross-ios-sim -- \
  podman run --rm -v "$PWD":/project -w /project \
    -v squallar-xcross-cargo:/usr/local/cargo/registry "$IMG" bash -c '
      export RUSTUP_TOOLCHAIN="${RUST_VERSION:?}"; export CARGO_BUILD_JOBS=4
      make -C packaging/ios IN_CONTAINER=1 SIMULATOR=1'
```

(`CONTAINERS_CONF_OVERRIDE` exported as below, or the cage will not reach it.)
The Makefile picks `target-ios/` itself; the `.app` lands under
`packaging/ios/build/linux/iphonesimulator/`.

## The cage — a podman container is outside `board.sh`'s scope unless you say so

Rootless podman puts a container in **its own** transient cgroup, not the
caller's. Measured 2026-09-12: a plain `podman run` started by `board.sh`
reads `memory.max=max pids.max=2048` from inside — podman's own scope, no
byte ceiling, and one `rd-memwatch` cannot see (it lists `rd-board-*` scopes
and would log `scopes=none` while the build ran). Two spellings keep the
container's processes in the board scope instead; with either, the same read
gives **`memory.max=30064771072`** (28 GiB) and **`pids.max=8192`**:

- `podman run --cgroups=disabled …` on the command line, or
- `CONTAINERS_CONF_OVERRIDE=$HOME/.cache/rd-board-lock/podman-caged.conf`
  in the environment, where that file is

  ```toml
  [containers]
  cgroups = "disabled"
  ```

  This is the one that reaches the Makefiles' own `podman run`, which you do
  not edit. Create the file if a reboot took it; it is not in the repo.

The lock is `board.sh`'s as usual: one heavy thing on the box at a time, both
sessions, announce before taking it.

## What a green here proves, and what it does not

- It proves the same thing CI's Build row proves: **compiles and links** for
  that triple, with that SDK, in that image. Nothing here **runs** an Apple or
  Windows binary — no simulator, no macOS host. A macOS run needs the Mac
  (`mac-browser-rig`); a Windows run needs `ssh sim` (`windows-rig`); iOS has
  no run arm at all yet.
- The image is a snapshot. The Assert step in `build.yaml` ("the rust-cross
  image no longer ships <target> std") exists because targets can drift on an
  image bump; when the digest in `build.yaml` moves, `podman pull` the new one
  by digest and update `IMG` here and in both Makefiles (they carry the same
  string).
- `target-ios/` in the shared checkout (1.4 GB, 2026-09-07) is a leftover
  from a local simulator build, not a cache anything reads. Disposable.

## Measured 2026-09-12, `e3dcc1389`, `-j 4`, registry volume warm, target dir cold

| row | command | result | wall |
|---|---|---|---|
| iOS device, the break | `cargo check -p squallar --lib --target aarch64-apple-ios` on `cffa82fea` | **RED**, `error[E0433]: cannot find DesktopPlatform in platform` — CI's error verbatim | 65 s |
| iOS device | `cargo check --workspace --all-targets --features squallar/jni-typecheck --target aarch64-apple-ios` | GREEN | 89 s (first triple into `target/xcross/`: pays the host-side build scripts and proc-macros once) |
| iOS simulator | same, `aarch64-apple-ios-sim` | GREEN | 31 s |
| macOS arm64 | same, `aarch64-apple-darwin` | GREEN | 31 s |
| macOS x86_64 | same, `x86_64-apple-darwin` | GREEN | 32 s |
| Windows | same, `x86_64-pc-windows-gnullvm` | GREEN | 40 s |
| iOS simulator link | `make -C packaging/ios IN_CONTAINER=1 SIMULATOR=1` inside the container (CI's row verbatim), `CARGO_BUILD_JOBS=4` | GREEN — release + fat LTO, staticlib linked against iphonesimulator26.4, `.app` assembled (`DTSDKName=iphonesimulator26.4`, `minos=15.0`) | 192 s |

So the whole Apple + Windows check set is **about four minutes** on top of a
lane's board, sharing one `target/xcross/`. There is no cost argument against
running it either.

Re-measure and replace this table when the image digest changes.
