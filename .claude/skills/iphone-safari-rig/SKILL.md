---
name: iphone-safari-rig
description: Measure squallar's web memory wall on the iPhone 13 Pro (or any iOS device) -- a Safari tab driven by safaridriver on the Mac mini over USB, and the home-screen web app no driver can reach -- with both reporting over HTTPS to the Linux box. Use for M1 wall legs on iOS, for serving the TLS + COEP page a phone trusts, and for turning a no-driver leg into a row with `drive.py analyze-console`.
---

# The iPhone wall rig (M1)

## Where this stands (2026-09-12)

The first device attempt got as far as creating a WebDriver session, and the
phone refused it:

```
session not created: Could not create a session: Some devices were found, but could not be used:
- iPhone (00008110-001108891192801E): device is not paired
```

**No calibrate or scene leg has run on the phone.** Everything before session
creation ran and is marked RAN below, with the date. Everything after it is
still UNRUN. The desktop halves of the same code (calibrate page,
`--until-death`, `analyze-console`, the console beacon, `RIG_CALIBRATE_JSONS`)
have run on Linux Firefox and Chromium. Treat an UNRUN command's first failure
as a fact about the command, not about the phone.

## The device

Read with `xcrun devicectl device info details` on the Mac (RAN 2026-09-12,
read-only):

| Field | Value |
|---|---|
| model | iPhone14,2 (iPhone 13 Pro), hardware model D63AP, arm64e |
| iOS | 26.6.1 (23G83) |
| CoreDevice identifier (the safaridriver UDID) | `64A825AA-AE5A-510E-88B0-E29F52F57C1D` |
| hardware UDID (what safaridriver's errors print) | `00008110-001108891192801E` |
| transport | wired |
| pairingState / tunnelState / ddiServicesAvailable | **unpaired** / disconnected / false |

The same command writes these facts as JSON for the evidence files (RAN):

```bash
ssh mac 'xcrun devicectl device info details --device 64A825AA-AE5A-510E-88B0-E29F52F57C1D --json-output /dev/stdout' \
  > /home/reddragon/.cache/lane-m1-wall-rig-out/iphone/devicectl-details.raw
```

The lane reduced it to `/home/reddragon/.cache/lane-m1-wall-rig-out/iphone/device-reported.json`,
which `RIG_DEVICE_INFO_JSON` copies into every evidence file.

## What runs where

| Piece | Where | Why there |
|---|---|---|
| `serve.py` (TLS + COEP, `--report-log`) | Linux box, `192.168.0.37` | the phone reaches it directly, so a killed tab's last step is on disk before the kill |
| `drive.py` | Linux box | it reads the report log at the end of a leg, so it runs beside it |
| `safaridriver` | Mac mini (`ssh mac`), macOS 26.4.1, Safari 26.4 | only a Mac drives an iPhone; started per leg over ssh |
| Safari tab / home-screen app | the iPhone | the device under test |

The phone reports independently of any driver: every calibrate step beacons
to `/rig/report`, and every memory console line beacons to `/rig/console`.

## Provisioning

**"USB-paired" was not enough, and that was wrong in the first version of this
file.** The phone TRUSTS the Mac over USB (lockdown pairing, "Trust This
Computer"), which is what devicectl's `available` and `wired` mean. safaridriver
needs the separate CoreDevice developer pairing, which reads `pairingState:
unpaired` here.

Precondition check, read-only, before any driven leg (RAN):

```bash
ssh mac 'xcrun devicectl list devices --json-output /dev/stdout' | grep -E '"(pairingState|tunnelState|ddiServicesAvailable)"'
```

`"pairingState" : "paired"` is required.

Done:
- Mac: `safaridriver --enable`.
- iPhone: Settings > Safari > Advanced > **Web Inspector ON** and **Remote Automation ON**; Auto-Lock **Never**; USB-trusted with the Mac.
- iPhone: the mkcert root, `$(mkcert -CAROOT)/rootCA.pem` (sha256 `63:99:B0:D3:…:8C:37`), was AirDropped, its profile installed, and full trust enabled in Certificate Trust Settings (the user, 2026-09-12).

**Pending, and it needs the user's hands AND the user's say-so on their Mac:**
1. CoreDevice pairing (UNRUN). On the Mac: `xcrun devicectl manage pair --device 64A825AA-AE5A-510E-88B0-E29F52F57C1D`. The phone then asks to Trust and for its passcode. This writes a pairing record on the Mac, which is the user's personal machine, so the user runs it or authorises it; a lane never does it on its own.
2. Possibly Developer Mode (UNVERIFIED whether safaridriver demands it once paired): iPhone Settings > Privacy & Security > Developer Mode > On, restart, confirm. Only if the next session creation names it.

Never run `mkcert -install` on the Linux box: it edits the box's trust store.

The leaf certificate for the LAN address (RAN 2026-09-12, valid until
2028-12-13, verifies against the mkcert root):

```bash
mkdir -p ~/.cache/rd-phone-tls
mkcert -cert-file ~/.cache/rd-phone-tls/lan.pem -key-file ~/.cache/rd-phone-tls/lan-key.pem \
  192.168.0.37 localhost 127.0.0.1
```

**LAN reachability** (RAN 2026-09-12, from the Mac): a `serve.py --host 0.0.0.0
--coep --tls-cert … --tls-key …` on an ephemeral port answered
`https://192.168.0.37:<port>/rig/calibrate.html` with HTTP 200 and the
COOP/COEP headers. The Linux box runs no firewall (firewalld, nftables, ufw
and iptables all inactive).

**Phone-side trust proof** (UNRUN: no page has loaded on the phone yet): open
`https://192.168.0.37:<port>/rig/calibrate.html` in Safari. There must be no
certificate interstitial, and the first log line must read
`crossOriginIsolated=true SharedArrayBuffer=true`. `false` means no COEP or no
secure context, and every figure after it describes a page with no shared
memory: stop, don't record.

## Build and serve

The build served is whatever `squallar-web/pkg` holds; `--skip-build` never
rebuilds. All cargo goes through the box lock:

```bash
cd /home/reddragon/.cache/lane-m1-wall-rig
~/.cache/rd-land-check/board.sh m1rig-wasm -- env CARGO_BUILD_JOBS=4 \
  .github/scripts/wasm-threads.sh wasm-pack build squallar-web --target web --release --no-typescript --no-pack
```

Run every script as `bash <file>`, never `source` it into a tool shell, and
never assign zsh special parameters (`path`, `status`, …) in tool-shell code.

## Arm 1: Safari tab, driven through safaridriver

`drive.py` starts its driver as `<driver> -p <port>` and asks `<driver>
--version`. This wrapper runs the Mac's safaridriver over ssh and forwards the
port. It lives outside the tree, at
`/home/reddragon/.cache/lane-m1-wall-rig-out/iphone/safaridriver-via-mac`:

```sh
#!/bin/sh
if [ "${1:-}" = "--version" ]; then
  exec ssh -o BatchMode=yes -o ConnectTimeout=10 mac /usr/bin/safaridriver --version
fi
if [ "${1:-}" != "-p" ] || [ -z "${2:-}" ]; then echo "usage: $0 -p PORT | --version" >&2; exit 64; fi
exec ssh -tt -o BatchMode=yes -o ConnectTimeout=10 -o ExitOnForwardFailure=yes \
  -o ServerAliveInterval=15 -L "$2:127.0.0.1:$2" mac /usr/bin/safaridriver -p "$2"
```

RAN 2026-09-12: `--version` answers `Included with Safari 26.4 (21624.1.16.11.4)`.
The tunnel carried a session request to safaridriver and its refusal back.
After the leg no safaridriver was left on the Mac (`ssh mac pgrep -lx
safaridriver` listed nothing): the pty HUP ends it. The driver log's
`channel 3: open failed: connect failed: Connection refused` is drive.py
polling `/status` before safaridriver listens, and is harmless.

**The legs, once pairing reads `paired`** (all UNRUN past session creation).
Shared environment:

```bash
cd /home/reddragon/.cache/lane-m1-wall-rig
I=/home/reddragon/.cache/lane-m1-wall-rig-out/iphone
export RIG_SAFARIDRIVER=$I/safaridriver-via-mac RIG_SAFARI_IOS_UDID=64A825AA-AE5A-510E-88B0-E29F52F57C1D \
  RIG_TLS_CERT=$HOME/.cache/rd-phone-tls/lan.pem RIG_TLS_KEY=$HOME/.cache/rd-phone-tls/lan-key.pem \
  RIG_SERVE_HOST=0.0.0.0 RIG_URL_HOST=192.168.0.37 RIG_ARM=hardware \
  RIG_DEVICE_CLASS=ios-iphone13pro-safari-tab RIG_CAL_TIMEOUT=900
```

Three calibrate runs, each on its own server port (so a fresh origin) and its
own automation session:

```bash
for n in 1 2 3; do RIG_OUT_DIR=$I/cal$n bash .github/browser-rig/run_wall_arm.sh --skip-build --scenes "" safari; done
```

Then the scenes, joined to the lowest-survived run, with the device facts
copied into evidence:

```bash
RIG_OUT_DIR=$I/scenes RIG_CALIBRATE_JSONS="$I/cal1/safari.calibrate.json $I/cal2/safari.calibrate.json $I/cal3/safari.calibrate.json" \
RIG_DEVICE_INFO_JSON=$I/device-reported.json \
  bash .github/browser-rig/run_wall_arm.sh --skip-build --scenes "WALL1 WALL4 WALL6" safari
```

Run each invocation as a background job (a job is reaped at 3600 s) and block
on its marker; never end a turn to wait. The summary prints each leg's driven
and beaconed wall row, its device signals and its job costs, writes
`$I/scenes/wall-evidence/<leg_id>.json`, and prints ledger rows.

**Fresh website data.** safaridriver cannot wipe Safari's website data, and on
this phone a Settings wipe needs hands. Each leg is therefore a new server port
(a new origin, whose storage starts empty) and a new automation session. The
calibrate page records `storage_at_load.local_storage_keys` in every step, and
it must read 0; a nonzero value is a run on dirty data. Whether iOS automation
sessions are also ephemeral is UNVERIFIED.

What to expect (UNVERIFIED on the device):
- A killed WebContent process may make Safari reload the tab once. `drive.py` counts a new document as the death, or a driver error naming a gone page, or three silent probes. The calibrate leg then loads the run again to read it back.
- WebKit refused a 2 GiB `shared` maximum and accepted 256 MiB in published reports; 512 MiB is untested. The ladder answers it here. heap.js classifies this phone as handheld and asks 512 MiB for the page and 256 MiB for the worker.
- If the largest constructing rung is below the 4 GiB cap, the growth memory is capped at that rung, and the run may end `refusal` with `at_maximum: true`. That bounds ONE memory at its own maximum, not the OS wall: say so beside the figure.
- The phone shows an automation banner. Do not touch it during a driven leg.
- Afterwards: `ssh mac pgrep -lx safaridriver` lists nothing, or you kill only the PIDs this leg started. The user's own Mac Safari may be running (pid 5711 on 2026-09-12); it is not the rig's. Leave it.

## Arm 2: the home-screen web app (no driver; the user's hands every leg)

This arm does not need CoreDevice pairing. Readings arrive over Wi-Fi. UNRUN.

Server, one scene at a time, on the FIXED port 8443, so the installed apps keep
one origin across scene changes:

```bash
cd /home/reddragon/.cache/lane-m1-wall-rig
RIG_TLS_CERT=$HOME/.cache/rd-phone-tls/lan.pem RIG_TLS_KEY=$HOME/.cache/rd-phone-tls/lan-key.pem \
RIG_SERVE_HOST=0.0.0.0 RIG_URL_HOST=192.168.0.37 RIG_SERVE_PORT=8443 \
RIG_OUT_DIR=/home/reddragon/.cache/lane-m1-wall-rig-out/iphone-pwa \
  bash .github/browser-rig/run_wall_arm.sh --serve-only WALL1
```

It prints the URLs and the report log path, then serves until stopped. Stop
it by the PID it runs as, never `pkill -f`. `--serve-only` passes
`--instrument-index`, so `/` (the manifest's `start_url`) carries the prelude,
the seed and the beacon. Changing scene means restarting it with the new name
on the same port; the installed app reads the new seed at its next launch.

The user, on the phone (same Wi-Fi as 192.168.0.37):
1. Once: Safari > `https://192.168.0.37:8443/` loads with no certificate warning > Share > **Add to Home Screen**.
2. Once: Safari > `https://192.168.0.37:8443/rig/calibrate.html` > Share > **Add to Home Screen** (the page declares itself home-screen capable).
3. Calibrate, three times: before each run, delete the calibrate icon and add it again from Safari (a home-screen app's website data goes with its icon). Close other apps, launch it, tap **Start a run**, and leave the phone alone until the page shows how the run ended or the app dies. If it dies, launch it once more: it reads the dead run back. Note the run's end line.
4. Scenes: close every app, launch the squallar icon, and leave it for 150 s or until it dies. **A hand relaunch reads as a death**, so say if one happened. Swipe it closed, and repeat for WALL4 and WALL6 after the server switches.
5. Keep the phone on power, Low Power Mode off, Auto-Lock Never.

Collection, on the Linux box. List the calibrate runs the log holds, then
analyse each scene joined to the lowest-survived one:

```bash
python3 - <log> <<'EOF'
import json, sys
for raw in open(sys.argv[1]):
    b = json.loads(raw).get("body") or {}
    if isinstance(b, dict) and b.get("kind") == "calibrate" and b.get("phase") in ("ended", "readback"):
        print(b.get("run"), b.get("phase"), b.get("end") or (b.get("readback") or {}).get("ended_by"),
              b.get("survived_mib") or (b.get("readback") or {}).get("survived_mib"))
EOF
python3 .github/browser-rig/drive.py analyze-console \
  --log <the scene's report log> --calibrate-log <the log holding the calibrate runs> --calibrate-run <run id> \
  --tag ios-iphone13pro-pwa.WALL1 --out /home/reddragon/.cache/lane-m1-wall-rig-out/iphone-pwa
```

That writes `<tag>.json` (the `wall` block a driven leg writes) and
`<tag>.samples.tsv` (a driven leg's columns, one row per `budget state:` tick).
The runner writes evidence files for DRIVEN legs only, so a PWA leg's evidence
file still needs writing from its `<tag>.json`.

A pasted Web Inspector console also works as `--log`. It carries no page clock
and no load boundaries, so its `deaths` reads null, never zero.

## Reading and recording

- iOS rows are their own arm. Never merge them with a desktop row, or the Safari-tab context with the home-screen context.
- `wall_mib` is the lowest calibrate survived figure on the SAME device, browser and context. `residue_mib = wall_mib - sum_hw_mib`, where `sum_hw_mib` is the highest page+worker `linear` pair on one telemetry tick. Negative means the app's linear memory alone passed the device's measured wall.
- A row goes into `.github/browser-rig/wall-ceilings.tsv` only for a leg that ran, with its evidence file copied from `<out>/wall-evidence/<leg_id>.json` into `.github/browser-rig/wall-evidence/`. `squallar-web/tests/wall_ledger.rs` holds the rules: header exact, ceilings never rise per key, and every row's figures match its evidence.
