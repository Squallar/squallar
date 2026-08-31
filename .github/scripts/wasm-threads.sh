#!/usr/bin/env bash
#
# Run one command against the browser's THREADED wasm configuration.
#
#   .github/scripts/wasm-threads.sh cargo check -p squallar-web --target wasm32-unknown-unknown
#   .github/scripts/wasm-threads.sh wasm-pack build squallar-web --target web --release
#
# ---------------------------------------------------------------------------
# THE NIGHTLY CARVE-OUT, AND EXACTLY HOW FAR IT REACHES
# ---------------------------------------------------------------------------
#
# `rust-toolchain.toml` pins `channel = "stable"` and MUST KEEP PINNING IT.
# Editing that file would move the entire workspace -- every CI row, Android,
# iOS, macOS, the native desktop build -- onto nightly, to buy a flag that only
# wasm32-unknown-unknown needs. This script overrides the channel through
# `RUSTUP_TOOLCHAIN` for the duration of ONE command instead, which is the same
# lever the container rows already pull (see the note at the bottom of
# `rust-toolchain.toml`: they set `RUSTUP_TOOLCHAIN` from the image's own
# `RUST_VERSION` to override `channel`). Nothing outside the process this
# script execs sees nightly.
#
# Two things here are nightly-only and neither has a stable spelling yet:
#
#   * `-Ctarget-feature=+atomics,+bulk-memory,+mutable-globals` -- `atomics` is
#     an unstable target feature, and rustc says so on every build
#     ("warning: unstable feature specified for `-Ctarget-feature`: `atomics`").
#     Without it the module has no shared memory and no `atomic.*`, so rayon
#     has nothing to build a pool on.
#   * `-Zbuild-std=std,panic_abort` -- the shipped `wasm32-unknown-unknown` std
#     is compiled WITHOUT atomics, and linking a non-atomics std into an
#     atomics module gives a module whose `std::sync` primitives are not the
#     ones the threads are using. std has to be rebuilt against the same
#     feature set, which is what `rust-src` is installed for.
#
# `build-std` is passed as `CARGO_UNSTABLE_BUILD_STD` rather than as a `-Z`
# flag on the command line, because these same flags have to reach cargo
# through `wasm-pack build` and `wasm-pack test`, which own their own cargo
# invocations. An env var reaches cargo wherever it is spawned from; a `-Z`
# would need a different pass-through spelling per wasm-pack subcommand (they
# differ -- see the note in `.github/workflows/web.yaml`).
#
# WHAT USES THIS: every row that compiles wasm32. `web.yaml` (Tier 1 and Tier
# 2), `build.yaml`'s `web-wasm32` row, `clippy.yaml`'s wasm32 verification
# check, and the local rig scripts `run_tier2.sh` / `run_gpu_arm.sh`. A wasm32
# build that does NOT go through here will fail loudly rather than quietly
# producing a single-threaded module: `wasm-bindgen-rayon` carries a
# `compile_error!` for a wasm32 target without `atomics`.
#
# WHAT DOES NOT USE THIS: everything else, which is most of the repo. The host
# test rows, the clippy rows, the Android/iOS/macOS builds and every native
# `cargo` command stay on the pinned stable toolchain.

set -euo pipefail

if [ "$#" -eq 0 ]; then
  echo "usage: $0 <command> [args...]" >&2
  exit 2
fi

# The channel, for this process tree only.
#
# DATED, not floating `nightly`, and the pin is load-bearing twice over. It is
# the usual reproducibility argument -- a floating nightly makes this the one
# input to the web build that changes without a commit -- and it is also a
# known-good marker. `-Zbuild-std` on wasm32-unknown-unknown was broken on
# older nightlies: rebuilding std left libm undefined, and the link died on
# `undefined symbol: acosh` / `asinh` out of naga, walkers and squallar-geo.
# Measured on this box at WS3b, on a three-arm probe over a two-line crate
# calling `f64::asinh`:
#
#   stable 1.97.1, shipped std, no atomics  -> links
#   1.96.0-nightly (2026-04-05), build-std  -> undefined symbol: acosh
#   1.99.0-nightly (2026-08-14), build-std  -> links
#
# The failure did NOT depend on `+atomics`; plain `-Zbuild-std` was enough to
# trigger it, which is why the arm above is worth keeping in the record. Moving
# this pin means re-running that probe, not assuming forward progress.
: "${SQUALLAR_WASM_TOOLCHAIN:=nightly-2026-08-15}"
export RUSTUP_TOOLCHAIN="$SQUALLAR_WASM_TOOLCHAIN"

# Additive, and required by `-Zbuild-std`: the toolchain has to carry the wasm
# target's own std even though std is about to be rebuilt from source, because
# cargo still resolves the target spec through it.
if ! rustup target list --toolchain "$RUSTUP_TOOLCHAIN" --installed 2>/dev/null | grep -qx "wasm32-unknown-unknown"; then
  echo "installing wasm32-unknown-unknown for $RUSTUP_TOOLCHAIN" >&2
  rustup target add wasm32-unknown-unknown --toolchain "$RUSTUP_TOOLCHAIN"
fi

# `build-std` needs the standard library's SOURCE, which is a rustup component
# and not part of a default toolchain install. Checked rather than assumed:
# without it cargo fails deep inside a std build with an error that reads like
# a compiler bug.
# `clippy` alongside it, because `rust-toolchain.toml` lists it as a component
# of the pinned channel and a carve-out that cannot run the linter would
# quietly exempt the wasm arm from it.
for component in rust-src clippy; do
  if ! rustup component list --toolchain "$RUSTUP_TOOLCHAIN" --installed 2>/dev/null | grep -qx "$component"; then
    echo "installing $component for $RUSTUP_TOOLCHAIN" >&2
    rustup component add "$component" --toolchain "$RUSTUP_TOOLCHAIN"
  fi
done

# The linker half, and it is NOT implied by `+atomics`. Measured at WS3b: with
# the target features alone the module came out carrying 14284 atomic
# instructions over a memory that was neither shared nor imported, and
# wasm-bindgen -- which decides whether to emit thread glue by looking for an
# imported memory -- generated a single-argument `initSync(module)`. A worker
# has no way to join a memory it cannot be handed, so that module is atomics
# over one private heap per instance: threads that cannot see each other.
#
#   --shared-memory   marks the memory `shared`, which is what makes a
#                     `SharedArrayBuffer` out of it (and what needs the
#                     COOP/COEP isolation the CloudFront Response Headers
#                     Policy on squallar.app emits).
#   --import-memory   moves the memory from the module's own Memory section to
#                     its Import section, so every extra instantiation is
#                     *given* the first one's memory instead of making its own.
#                     This is the flag wasm-bindgen keys the thread glue off.
#   --max-memory      required by wasm-ld whenever the memory is shared: a
#                     shared memory cannot be relocated on growth, so its
#                     ceiling has to be known at link time and the engine
#                     reserves that much ADDRESS space up front. 1 GiB is
#                     rustc's own figure when it passes these itself; the
#                     reservation is virtual, not resident.
#
# The four `--export=` flags hand wasm-bindgen the linker-synthesized globals
# its threading transform rewrites. They are not exported by default and
# `--gc-sections` has no reason to keep them, so without these the transform
# stops with `failed to find __heap_base for injecting thread id` -- which is
# wasm-bindgen refusing to guess where a per-thread stack may be carved from,
# not a missing feature.
LINK_ARGS="-Clink-arg=--shared-memory -Clink-arg=--import-memory -Clink-arg=--max-memory=1073741824"
LINK_ARGS="$LINK_ARGS -Clink-arg=--export=__heap_base"
LINK_ARGS="$LINK_ARGS -Clink-arg=--export=__tls_base"
LINK_ARGS="$LINK_ARGS -Clink-arg=--export=__tls_size"
LINK_ARGS="$LINK_ARGS -Clink-arg=--export=__tls_align"
LINK_ARGS="$LINK_ARGS -Clink-arg=--export=__wasm_init_tls"

# Appended, not assigned: a caller that already set RUSTFLAGS (CI sometimes
# does) keeps what it set. Assigning would silently drop it.
export RUSTFLAGS="${RUSTFLAGS:-} -Ctarget-feature=+atomics,+bulk-memory,+mutable-globals $LINK_ARGS"
export CARGO_UNSTABLE_BUILD_STD="std,panic_abort"

exec "$@"
