---
name: mac-browser-rig
description: Run squallar browser measurement legs on the Mac (jacobs-mac-mini). Use when the Linux box's headed arm is unusable, when a second hardware arm is wanted, or when Safari/WebKit is the target. Covers reach, what is already installed, what must be installed, and the traps that have cost hours.
---

# Measuring on the Mac

`ssh mac` — jacobs-mac-mini, Apple M2, 10 GPU cores, 8 GiB unified, macOS 26.4.1
arm64. No VPN, no wake step, `BatchMode=yes` works.

**Browser legs run headed over a plain ssh session** — verified: rAF fires at
16.67 ms on the 60 Hz panel, which is presentation, not boot. webdriver launches
the browser itself, which is why this works.

**The NATIVE app over ssh is UNVERIFIED and should not be assumed.** An earlier
note here claimed it runs headed with no `launchctl asuser`, citing `Surface
configured to 1920x1018` and `wgpu selected the Metal backend`. Those are BOOT
signals, not presentation signals — the same class of evidence that reads green
in the Windows session-0 trap, where the app creates its window, spins its event
loop and autosaves its config while `RedrawRequested` never fires.

There is also a reason to expect trouble: WindowServer access is gated on the
`gui/$UID` bootstrap domain and an ssh session lands in `user/$UID`, which is
what `launchctl asuser` exists to bridge. "It just works" needs an explanation
and nobody has produced one.

Before running a native leg here, prove PRESENTATION and not boot: frames
actually cadencing, a non-empty frame telemetry window — not a surface line.

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
confusing failure of a build that already worked. Nothing in the message names
`RUSTFLAGS`.

**Do NOT pre-install into wasm-pack's own cache.** wasm-pack runs `cargo install
--force`, and a `--force` that then fails **deletes that directory** — including
a good binary put there beforehand. Tried: the whole
`~/Library/Caches/.wasm-pack/.wasm-bindgen-cargo-install-<version>/` tree was
gone afterwards, and the next run said `Not able to find or install a local
wasm-bindgen`, which reads as a missing tool rather than a deleted one.

Install it where wasm-pack does not manage anything, and tell it not to try:

```bash
env -u RUSTFLAGS -u CARGO_UNSTABLE_BUILD_STD -u RUSTUP_TOOLCHAIN \
  cargo install wasm-bindgen-cli --version <version> --root ~/rd-<lane>-arm

PATH=~/rd-<lane>-arm/bin:$PATH .github/scripts/wasm-threads.sh \
  wasm-pack build squallar-web --mode no-install ...
```

`--mode no-install` is load-bearing: without it wasm-pack reinstalls over
whatever is already there. Take the version from the error text.

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
