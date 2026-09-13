---
name: iphone-safari-rig
description: Measure squallar's web memory wall on the iPhone 13 Pro (or any iOS device) -- a Safari tab driven by safaridriver on the Mac mini over USB, and the home-screen web app no driver can reach -- with both reporting over HTTPS to the Linux box. Use for M1 wall legs on iOS, for serving the TLS + COEP page a phone trusts, and for turning a no-driver leg into a row with `drive.py analyze-console`.
---

# The iPhone wall rig (M1)

Written 2026-09-12 by the M1 wall-rig lane, before any iOS leg had run. **Every
command marked UNRUN has never been executed from this tree**; the desktop
halves of the same code (calibrate page, `--until-death`, `analyze-console`,
the console beacon) were run on Linux Firefox and Chromium first. Treat an
UNRUN command's first failure as a fact about the command, not the phone.

## What runs where

| Piece | Where | Why there |
|---|---|---|
| `serve.py` (TLS + COEP, `--report-log`) | Linux box, `192.168.0.37` | the phone must reach it directly: a killed tab's last step has to be on disk before the kill |
| `drive.py` | Linux box | it reads the report log at the end of a leg, so it runs beside it |
| `safaridriver` | Mac mini (`ssh mac`) | Apple ships it; only a Mac drives an iPhone |
| Safari tab / home-screen app | iPhone 13 Pro, udid `64A825AA-AE5A-510E-88B0-E29F52F57C1D` | the device under test |

The phone reports twice, independently of any driver: every calibrate step
beacons to `/rig/report`, every memory console line beacons to `/rig/console`.
Both land as JSON lines in the `--report-log` file on the Linux box.

## Provisioning

Already done (the user's word, 2026-09-12):
- Mac: `safaridriver --enable`.
- iPhone: Settings > Safari > Advanced > **Web Inspector ON** and **Remote Automation ON**; Auto-Lock **Never**; USB paired and trusted with the Mac.

**Pending, and it needs the user's hands:** trusting the mkcert root on the phone.
1. From the Linux box, send `$(mkcert -CAROOT)/rootCA.pem` (it is `~/.local/share/mkcert/rootCA.pem`) to the phone (AirDrop or mail). **Never run `mkcert -install` on the box**: that edits the box's system trust store.
2. Phone: Settings > General > VPN & Device Management > install the downloaded profile.
3. Phone: Settings > General > About > Certificate Trust Settings > switch on full trust for the mkcert root.

The leaf certificate for the LAN address, on the Linux box (UNRUN):

```bash
mkdir -p ~/.cache/rd-phone-tls
mkcert -cert-file ~/.cache/rd-phone-tls/lan.pem -key-file ~/.cache/rd-phone-tls/lan-key.pem \
  192.168.0.37 localhost 127.0.0.1
```

**Proof the trust took, read off the glass** (UNRUN): with a server up (next
section), open `https://192.168.0.37:8443/rig/calibrate.html` in Safari. There
must be no certificate interstitial, and the first log line must read
`crossOriginIsolated=true SharedArrayBuffer=true`. `false` there means no COEP
or no secure context, and every figure after it describes a page with no shared
memory: stop and fix it rather than record it.

## Build and serve (Linux box)

All cargo goes through the box lock. From the lane worktree:

```bash
cd /home/reddragon/.cache/lane-m1-wall-rig
~/.cache/rd-land-check/board.sh m1rig-wasm -- env CARGO_BUILD_JOBS=4 \
  .github/scripts/wasm-threads.sh wasm-pack build squallar-web --target web --release --no-typescript --no-pack
```

Serve ONE scene on the LAN, with the calibrate page beside it (UNRUN):

```bash
cd /home/reddragon/.cache/lane-m1-wall-rig
RIG_TLS_CERT=$HOME/.cache/rd-phone-tls/lan.pem RIG_TLS_KEY=$HOME/.cache/rd-phone-tls/lan-key.pem \
RIG_SERVE_HOST=0.0.0.0 RIG_URL_HOST=192.168.0.37 RIG_SERVE_PORT=8443 \
RIG_OUT_DIR=/home/reddragon/.cache/lane-m1-wall-rig-out/iphone \
  .github/browser-rig/run_wall_arm.sh --serve-only WALL1
```

It prints the scene URL, the home-screen URL, the calibrate URL and the report
log path, then serves until Ctrl-C. Only one scene is seeded per server;
another scene means stopping it and starting it again with the new name. It is
a foreground process: run it in a terminal the user owns (a harness background
job is reaped at 3600 s). Stop it with Ctrl-C, or kill **the PID it runs as**,
never `pkill -f`.

The server needs the LAN to reach port 8443 (`ss -ltnp | grep 8443`, then load
the page from the phone). The scene page, the home-screen app and the calibrate
page share one origin, so they share localStorage; the calibrate page's keys
(`squallar.rig.calibrate.a`/`.b`) collide with nothing the app writes.

## Arm 1: Safari tab, driven through safaridriver

`drive.py` starts its driver as a local process listening on a local port. A
wrapper that tunnels that port to the Mac makes the Mac's safaridriver look
local, so the whole leg, report log included, runs from the Linux box
(UNRUN):

```bash
mkdir -p /home/reddragon/.cache/lane-m1-wall-rig-out/iphone
cat > /home/reddragon/.cache/lane-m1-wall-rig-out/iphone/safaridriver-via-mac <<'EOF'
#!/bin/sh
# drive.py runs `<driver> -p <port>`. Forward that port to the Mac's own safaridriver.
# -tt gives the remote a tty, so it gets SIGHUP when drive.py kills this ssh.
exec ssh -tt -o BatchMode=yes -o ExitOnForwardFailure=yes -L "$2:127.0.0.1:$2" mac /usr/bin/safaridriver -p "$2"
EOF
chmod +x /home/reddragon/.cache/lane-m1-wall-rig-out/iphone/safaridriver-via-mac
```

The whole Safari-tab arm, calibrate plus the three scenes (UNRUN):

```bash
cd /home/reddragon/.cache/lane-m1-wall-rig
RIG_SAFARIDRIVER=/home/reddragon/.cache/lane-m1-wall-rig-out/iphone/safaridriver-via-mac \
RIG_SAFARI_IOS_UDID=64A825AA-AE5A-510E-88B0-E29F52F57C1D \
RIG_TLS_CERT=$HOME/.cache/rd-phone-tls/lan.pem RIG_TLS_KEY=$HOME/.cache/rd-phone-tls/lan-key.pem \
RIG_SERVE_HOST=0.0.0.0 RIG_URL_HOST=192.168.0.37 \
RIG_DEVICE_CLASS=iphone13pro RIG_OUT_DIR=/home/reddragon/.cache/lane-m1-wall-rig-out/iphone \
  .github/browser-rig/run_wall_arm.sh --skip-build --scenes "WALL1 WALL4 WALL6" safari
```

One calibrate leg by hand, when only the ladder and the wall are wanted (UNRUN):

```bash
python3 .github/browser-rig/serve.py --dir squallar-web --port 8443 --host 0.0.0.0 --coep \
  --tls-cert ~/.cache/rd-phone-tls/lan.pem --tls-key ~/.cache/rd-phone-tls/lan-key.pem \
  --report-log /home/reddragon/.cache/lane-m1-wall-rig-out/iphone/cal.report.jsonl &
SERVE_PID=$!
python3 .github/browser-rig/drive.py --browser safari --arm hardware \
  --driver /home/reddragon/.cache/lane-m1-wall-rig-out/iphone/safaridriver-via-mac \
  --safari-ios 64A825AA-AE5A-510E-88B0-E29F52F57C1D \
  --url "https://192.168.0.37:8443/rig/calibrate.html?run=ios-$(date +%s)&cap_mib=4096&step_mib=32" \
  --calibrate --report-log /home/reddragon/.cache/lane-m1-wall-rig-out/iphone/cal.report.jsonl \
  --calibrate-timeout 900 --out-dir /home/reddragon/.cache/lane-m1-wall-rig-out/iphone --tag iphone13pro.safari.calibrate
kill "$SERVE_PID"
```

What to expect, and what is not established yet:
- A killed WebContent process makes Safari **reload the tab once**. `drive.py` reads the new document as the death, and the reloaded calibrate page reads the dead run back. A second kill shows Safari's "A problem repeatedly occurred" page instead; the leg has already ended at the first.
- WebKit refused a 2 GiB `shared` maximum and accepted 256 MiB in published reports; 512 MiB is untested. The ladder answers it on this device.
- If the largest rung that constructs is below the cap, the growth memory is capped at that rung and the run ends `refusal` with `at_maximum: true`. That reading bounds ONE memory at its own maximum, not the OS wall: say so beside the figure.
- Driven by safaridriver, the phone shows the automation banner. **Do not touch the phone during a driven leg**: a touch ends the automation session.
- Wiping Safari data between legs is allowed (Settings > Safari > Clear History and Website Data), and changing its settings is allowed: this phone is a lab device. Keep it on USB power, Low Power Mode off, with no other tabs or apps open. Put the iOS version and the Safari version from the row's `session.browserVersion` into every figure's denominator.
- If the tunnel wrapper fails, the fallback is `drive.py` on the Mac itself (stdlib Python; see `mac-browser-rig` for getting a tree over). That run cannot read the report log, so its calibrate verdict notes "only the localStorage route answered"; cross-check the step records with `analyze-console` on the Linux box afterwards.
- Afterwards check the Mac for a leftover driver: `ssh mac pgrep -lx safaridriver`. Kill only the PIDs this leg started. A Safari `--automation` process from another session is to be noted, not killed.

## Arm 2: the home-screen web app (no driver; the user's hands every leg)

**Needs the user's hands every leg:** installing to the home screen, launching,
and saying when a leg started and stopped.

1. Linux: `run_wall_arm.sh --serve-only WALL1` (above), and note the report log path it prints.
2. Phone, once: open `https://192.168.0.37:8443/` in Safari. `--serve-only` passes `--instrument-index`, so `/` carries the rig prelude and the beacon. Share > **Add to Home Screen**. The manifest's `start_url` is `./`, so the installed app always opens the scene the server currently seeds, and one install serves every scene.
3. Phone, per leg: close every other app, launch the home-screen icon, and leave it alone for 150 s or until it dies. **Every page load after the first counts as a death**, and a relaunch by hand looks exactly like one, so write down any hand relaunch next to the leg.
4. Calibrate in the same standalone context: in Safari, open `https://192.168.0.37:8443/rig/calibrate.html`, Add to Home Screen (the page declares itself home-screen capable), launch it and press **Start a run**. After a death iOS relaunches it into the readback; otherwise it shows the end on the glass.
5. Linux: turn the logs into rows (UNRUN):

```bash
cd /home/reddragon/.cache/lane-m1-wall-rig
python3 .github/browser-rig/drive.py analyze-console \
  --log <the scene's report log> --calibrate-log <the calibrate session's report log> --calibrate-run latest \
  --tag iphone13pro.pwa.WALL1 --out /home/reddragon/.cache/lane-m1-wall-rig-out/iphone
```

That writes `iphone13pro.pwa.WALL1.json` (the `wall` block a driven leg writes: page and worker linear high-water, ceilings, the census at the last tick, deaths, residue) and `iphone13pro.pwa.WALL1.samples.tsv` (a driven leg's columns, one row per `budget state:` tick). `--calibrate-run latest` reads the calibrate verdict out of the log itself: the step records are the `/rig/report` route, and the page's readback record, which carries what localStorage held, is the other.

A pasted Web Inspector console (Develop > iPhone > the page > Console > save) also works as `--log`. It carries no page clock and no load boundaries, so its `deaths` reads null, never zero.

## Reading and recording

- iOS rows are their own arm. Never merge them with a desktop row or with each other across Safari-tab and home-screen contexts.
- `wall_mib` is the calibrate survived figure on the SAME device, browser and context. `residue_mib = wall_mib - sum_hw_mib`, where `sum_hw_mib` is the highest page+worker `linear` pair on one telemetry tick. Negative means the app's linear memory alone passed the device's measured wall.
- A row goes into `.github/browser-rig/wall-ceilings.tsv` only for a leg that ran, with its evidence file copied from `<out>/wall-evidence/<leg_id>.json` into `.github/browser-rig/wall-evidence/`. `squallar-web/tests/wall_ledger.rs` holds the rules: header exact, ceilings never rise per key, and every row's figures match its evidence.
