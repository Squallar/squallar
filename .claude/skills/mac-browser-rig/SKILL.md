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

**The native app also reaches its render loop over ssh, with no `launchctl
asuser` and no sudo** — on presentation evidence, not on a surface line. Three
legs, `frame cadence`:

| scene | samples | p50 |
|---|---|---|
| A | n=5906 | 19028 us |
| C | n=8754 | 19028 us |
| E2 | n=11934 | 19028 us |

19,028 us is 52.6 Hz against that box's 60 Hz panel, sustained for minutes, with
a populated concentrated histogram climbing monotonically across 75 readings.
Compare the dead-display signature on the Linux box — `cadence n=182 p50=OVER` —
roughly forty-eight times fewer frames from the same field. Also `overlay
pictures` filling in real pixel dimensions and resident bytes two seconds after
starting at `px=0x0, bytes=0`, and 74 `budget state:` lines, which are composed
only after GPU init inside the redraw path.

**What is NOT established: nobody looked at the glass.** No screenshot was taken
in any of those legs. The claim that survives is "reached and sustained its
render loop at panel-adjacent cadence", not "a scene was seen to render". If you
need the second thing, capture an image; do not infer it from this.

**The mechanism is unexplained and that is worth recording rather than
papering over.** WindowServer access is gated on the `gui/$UID` bootstrap domain,
an ssh session lands in `user/$UID`, and `launchctl asuser` exists to bridge
exactly that — so the prediction was trouble and the observation was none. The
most likely hidden precondition is that the console user is logged in AND
unlocked; record that state on any future leg.

**And if it ever stops working, `launchctl asuser $UID` is the remedy, not a
workaround to avoid.** Nothing here argues against using it — the finding is only
that it was not needed, on this box, in this state. A lane that gets a wedged app
(window created, `frame cadence` stuck at `n=0`, no `budget state:` lines) should
reach for it early rather than concluding the box cannot run headed. Say in your
report whether you needed it, because that is the observation that would explain
the mechanism none of us can currently account for.

Note the browser path says nothing about this one: webdriver launches the browser
itself, so those legs never tested the ssh-to-WindowServer question.

The evidence that does NOT settle this, and did not: `Surface configured to
...` and `wgpu selected the Metal backend`. Both are boot signals. The Windows
box produces their equivalents and never presents a frame — see `windows-rig`.

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

### A Firefox leg that dies in ~4 s with rc=0 is the UPDATER, not the rig

`firefox exited rc=0 before Marionette came up`, a 0-byte `D.firefox.firefox.log`,
`adapter=None viewport=0x0`. The rig launches the user's INSTALLED
`/Applications/Firefox.app`, and the first launch after Firefox has staged an
update applies it and re-execs — the launched pid exits 0 and Marionette never
comes up. Observed 2026-09-04: bundle mtime moved to 12:12, version 155.0 →
155.0.1 under a 12:45 launch, the leg failed, the retry passed.

**And the same text has a SECOND cause that persists after the update is done.**
The rig's launch that "exited rc=0" can re-exec and keep running under a new pid
— `--marionette -no-remote -profile ~/rd-<lane>-ab/<hash>/…` — holding that
profile's lock. Every later launch into the same profile dir then exits 0 in ~4 s
with the identical message, and a retry only passes if it happens to get a fresh
profile path. Before blaming the updater twice, list Firefox processes carrying
the rig's OWN `-profile` path and kill only those (they are the rig's; the user's
Firefox has no such args). Observed 2026-09-04: pid launched 12:45 blocked the
13:01 leg; killing it fixed the 13:05 one.

Two consequences: **the browser version moves mid-campaign** — read
`browser_version` off every row rather than trusting what was installed when the
leg was queued; and **the user's own running Firefox may prompt to restart**,
because it is still on the old image. Tell them; do not kill it. The rig's
`-no-remote -profile <throwaway>` means it never touches their profile, and a
running user Firefox is NOT what blocked the leg — check the bundle mtime first.

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
