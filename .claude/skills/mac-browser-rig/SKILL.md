---
name: mac-browser-rig
description: Run squallar browser measurement legs on the Mac (jacobs-mac-mini). Use when the Linux box's headed arm is unusable, when a second hardware arm is wanted, or when Safari/WebKit is the target. Covers reach, what is already installed, what must be installed, and the traps that have cost hours.
---

# Measuring on the Mac

`ssh mac` — jacobs-mac-mini, Apple M2, 10 GPU cores, 8 GiB unified, macOS 26.4.1
arm64. No VPN, no wake step, `BatchMode=yes` works.

**The app runs HEADED over a plain ssh session** when the same user is logged in
at the console — no `launchctl asuser`, no sudo. A six-pane scene rendered that
way with `wgpu selected the Metal backend: Apple M2 (IntegratedGpu)`. This is the
opposite of the Windows box; see the `windows-rig` skill for why.

## What is ALREADY there — do not install these

Verified 2026-09-04:

| Thing | Where | Version |
|---|---|---|
| geckodriver | `~/ffmac-lane/bin/geckodriver` | 0.37.1 |
| chromedriver | `~/ffmac-lane/bin/chromedriver` | 152.0.7977.64 |
| Firefox | `/Applications/Firefox.app` | 155.0 |
| Chrome | `/Applications/Google Chrome.app` | 152.0.7977.76 (chromedriver matches) |
| safaridriver | `/usr/bin/safaridriver` | system |
| Xcode, rustup, cargo | system / `~/.cargo/bin` | Xcode 21 |

**`~/ffmac-lane/` is not ours.** Treat it read-only: read the two drivers out of
it, write nothing into it.

A previous briefing claimed geckodriver was absent and that a Mac arm would
therefore be Chromium-only and not quotable as the governing figure. **That was
wrong** — Firefox is installed and Firefox governs. Check before concluding.

## What must be installed

Only these two. Keep them in a directory you create (`~/rd-<lane>-arm/`) so the
box stays cleanable.

```bash
rustup toolchain install nightly-2026-08-15 --profile minimal \
  --component rust-src --component clippy --target wasm32-unknown-unknown

curl -fsSL -o wp.tar.gz \
  https://github.com/wasm-bindgen/wasm-pack/releases/download/v0.15.0/wasm-pack-v0.15.0-aarch64-apple-darwin.tar.gz
tar xzf wp.tar.gz && cp wasm-pack-v0.15.0-aarch64-apple-darwin/wasm-pack ~/rd-<lane>-arm/bin/
```

Two traps, each of which costs a round trip:

- **`--component rust-src clippy` FAILS.** rustup parses the second value as a
  toolchain name (`invalid toolchain name: 'clippy'`). Repeat the flag.
- **wasm-pack moved org.** `rustwasm/wasm-pack` 404s. The releases are at
  **`wasm-bindgen/wasm-pack`**. There is **no Homebrew** on this box, so the
  tarball is the only path.

### wasm-bindgen-cli must be installed SEPARATELY, with a clean environment

`wasm-pack` shells out to `cargo install wasm-bindgen-cli` when it cannot find a
matching one — and it inherits the environment `wasm-threads.sh` set up for
wasm32. Those `RUSTFLAGS` carry `-Clink-arg=--shared-memory` and friends, which
reach the linker for a **host** binary and kill it:

```
error: linking with `cc` failed: exit status: 1
Error: Installing wasm-bindgen with cargo
```

The wasm build itself succeeds first (295 crates), so this reads as a late,
confusing failure of a build that already worked. Install it up front, with the
wasm variables stripped, into the exact directory wasm-pack looks in:

```bash
env -u RUSTFLAGS -u CARGO_UNSTABLE_BUILD_STD -u RUSTUP_TOOLCHAIN \
  cargo install wasm-bindgen-cli --version <the version wasm-pack asks for> \
  --root "$HOME/Library/Caches/.wasm-pack/.wasm-bindgen-cargo-install-<version>"
```

The version is in the error text — take it from there rather than guessing, and
note the cache directory is wasm-pack's own, so it counts as yours to clean up.

The pinned nightly is not optional: `.github/scripts/wasm-threads.sh` pins
`nightly-2026-08-15` and installs the *target* and *components* itself, but it
cannot install the *toolchain*.

## Getting a tree over

No clone, no git remote — **nothing AI-driven pushes**. Copy a tarball into a
directory you create, with a `PROVENANCE.txt` recording the base hash, and
remove the directory when done. Do not reuse another lane's directory name.
Budget ~1.4 GiB for source + target + legs.

## House rules on this box

- Redirect `XDG_CONFIG_HOME` and `XDG_CACHE_HOME` into your own directory.
  `~/.config/squallar` and `~/.cache/squallar` are **the user's** — neither read
  nor write them.
- A leftover Safari `--automation` process may exist from another session.
  **Note it, do not kill it.**
- Verify your cleanup afterwards and say what you removed.

## Reading the results

- Mac figures are a **third arm**. Never merge them with headed-Linux or with
  headless Tier-2 — different GPU, compositor, refresh, and unified memory.
- Measured GPU capacity is 5,461 MiB and the budget ladder shed no rung on any
  scene tried (`steps 0`). An 8 GiB M2 is **not** a constrained device for our
  budgets; if a scene behaves differently there, capacity is not why.
- Firefox governs over Chrome, on whichever box.
