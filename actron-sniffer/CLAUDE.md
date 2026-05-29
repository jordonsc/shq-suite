# Actron RS485 Sniffer (experimental)

RS485 tool (sniffer **+ controller emulator**) for the **Actron NEO ↔ indoor-unit RS485 bus**
(the proprietary "IDU Interface"). Captures the Modbus traffic and can emulate a wall controller
to issue **local write commands** — the path to bypassing the unreliable cloud. Recon/prototype
hardware, not yet a deployed component. Originally built to answer "does the NEO put per-zone
setpoints on the bus?" — **yes** (and much more, see below).

It captures framed hex into a RAM ring buffer and serves it over an **open, LAN-only HTTP
server** so the capture can be observed and driven remotely (no USB babysitting). See
[`../docs/actron-local-control.md`](../docs/actron-local-control.md) for the wider
investigation.

**Status:** **deployed in the wall; full control map decoded.** Answer to the founding
question: **yes — the NEO puts per-zone setpoints, mode, fan, main setpoint, and a zone-enable
mask on the bus** (it's Modbus RTU 9600/8N1; values big-endian). The complete register map is in
[`FINDINGS.md`](FINDINGS.md) §7. Reachable at `http://REDACTED-IP/` (pinned) /
`redacted.local`. WiFi creds baked in (SHQ). `FRAME_MAX=512` (whole frames), OTA works.

**✅ Local write control achieved (2026-05-30, FINDINGS §9).** Emulate a **secondary controller
at 0x67** (answer its page-1 poll) and send a **command pulse** — new value in reg 12/56 **+
`reg 14 = 4` for one response cycle** — and the indoor board **adopts, commits, and broadcasts**
the change (proven: setpoint 24.0 → 22.0, persisted after disarm). **Non-invasive: the real NEO
stays live at 0x66.** Key insight: the board owns authoritative state and **ignores reported
values** (every static-value test failed) but **honours command pulses**. 0x67/0x68 are
page-1-only secondary slots. Command codes mapped: **mode=reg14:1, fan=2, setpoint=4**.
**Open:** (a) **zone setpoints** — page-2 values, no pulse; not settable from 0x67 (needs the
0x66/MITM path or the freshness-counter test, FINDINGS §9); (b) command codes for on/off,
zone-enable, away/turbo/cont-fan (quick); (c) build the emulator + HA integration. Firmware is
receive-only unless `/armwrite` is called (TX is firmware-gated); `/armwrite` takes `addr=`,
`ovr=`, `pulse=reg:val`, `pulsen=N`, `turn=`.

## Hardware

| Part | Notes |
|------|-------|
| Unexpected Maker **TinyC6** | ESP32-C6, USB-C, 3.3V logic. Board id `um_tinyc6`. |
| **3.3V** RS485 transceiver | **MAX3485 / SP3485 / THVD1410**. **Not a 5V MAX485** — its RO drives 5V into the C6 GPIO and kills the pin. |

On a parallel **tap**, the transceiver must **not** terminate the bus: if your module has a
120Ω A–B termination resistor, **remove it** (we're bridging in mid-bus, not capping an end).

## The NEO RJ45 — confirmed wiring

The NTW-1000 connects to the indoor unit over one RJ45 (Yellow Cat5E) carrying RS485 + 12V.
Per the NEO install guide, by **wire colour → function**:

| Function | Actron colour | RJ45 pin |
|----------|---------------|----------|
| **485 A** (D+) | **Blue** | **4** |
| **485 B** (D−) | **White/Blue** | **5** |
| +12V power | Brown **&** Orange | 8 (+ orange pin) |
| 0V / GND | White/Brown **&** White/Orange | 7 (+ white-orange pin) |
| unused | Green & White/Green | — |

The **blue pair is pins 4 & 5 in both T568A and T568B**, so 485 A/B are standard-independent
— this is the reliable bit. The orange/green pin numbers shift between T568A/B, so don't rely
on them; we only need pins **4, 5, 7**.

**Verify with a multimeter before powering the transceiver:** ~12V DC between pin 8 (brown,
+) and pin 7 (white/brown, GND). If polarity/colours differ (re-crimped cable), trust the
meter. A/B polarity isn't critical for sniffing — if frames look inverted/garbled, swap 4↔5.

## Pass-through tap harness (NEO stays live)

Bridge inline so all 8 conductors pass **straight through** wall ↔ NEO, and tap pins 4/5/7
in parallel to the transceiver:

```
  wall RJ45 ─────────────┬───────────── NEO RJ45     (all 8 pins straight through:
                         │                            pin N -> pin N, NEO fully powered)
            tap in parallel:
              pin 4 (blue) ........ transceiver A
              pin 5 (white/blue) .. transceiver B
              pin 7 (white/brown) . transceiver GND  + TinyC6 GND
              pin 8/+12V ........... DO NOT connect to the transceiver
```

Then the **auto-direction** TTL↔RS485 module: `RXD/DI ← GPIO16` (board "TX"), `TXD/RO → GPIO17`
(board "RX"), plus 3V3/GND. **No DE/RE pin** — the module keys its driver off UART TX. UART1 is
matrix-routed to these pins — still a hardware UART, not bit-banged.

Easiest physical build: two RJ45 breakout boards (or two keystone jacks wired pin-to-pin as a
coupler), with short flying leads off pins 4/5/7 to the transceiver. Keep the tap stub short.

**Power the TinyC6 from a USB-C charger** (power only — the whole point is to drop the data
cable). Optionally run it from the bus 12V via a 12→5V buck into the 5V pin; not needed for a
bench experiment.

**Safety:** receive-only during mapping. With an auto-direction module the driver keys on UART
TX, so "don't transmit" is enforced in *firmware* — the sniffer never writes to `Bus`. Don't add
bus writes until the protocol is understood and we deliberately enter the 0x67-reply phase (the
NEO is bus master; stray transmits collide and can error the A/C).

## WiFi credentials

Baked into `src/main.cpp` (throwaway experiment, LAN only) — currently set to the **SHQ**
network. `HOSTNAME = "actron-sniffer"` → advertised as `http://redacted.local/`.

## Build & flash

```bash
cd actron-sniffer
pio run && pio run -t upload     # build + flash over USB-C (first build pulls the toolchain — large)
pio device monitor               # optional: see the IP it got on boot
```

ESP32-C6 needs the **pioarduino** platform fork (pinned in `platformio.ini`; bump the tag from
<https://github.com/pioarduino/platform-espressif32/releases> if it 404s).

## HTTP API (observe + drive over LAN)

| Endpoint | Purpose |
|----------|---------|
| `GET /` | help + status |
| `GET /stats` | one-line status incl. `seq_max` |
| `GET /log?since=<seq>&n=<max>` | frames; `since` returns only newer ones (incremental polling) |
| `GET /set?baud=&parity=N\|E\|O&gap=<us>` | change capture settings live |
| `GET /measure` | estimate baud from raw line pulse widths (~5s; needs bus activity) |
| `GET /clear` | reset ring + counters |
| `GET /update?url=<bin>` | **HTTP-pull OTA** — download firmware from a URL and self-flash (see Remote reflash) |
| `GET /armwrite?addr=&ovr=reg:val,...&pulse=reg:val&pulsen=N&turn=<us>` | **ARM controller emulation.** `addr` = slot to emulate (0x66/0x67/**0x68**; default 0x67; 0x66 needs the real NEO unplugged). `ovr` = persistent register overrides on the cached 0x66 template (big-endian). `pulse=reg:val` = one-shot command pulse applied to the next `pulsen` page-1 responses then reverts (e.g. `pulse=14:4` = setpoint command edge). `turn` = reply turnaround µs (default 5000, > t3.5 ≈3.65 ms). **Setpoint write:** `addr=0x67&ovr=12:220,56:220&pulse=14:4` |
| `GET /disarm` | stop transmitting (back to receive-only) |
| `GET /txprobe` | TX self-test: inject a poll to 0x66 and report whether it answers |

Frame line: `<seq> <t_s> +<gap>us <len>: HEX...  |ascii|`

**Incremental polling** (how to watch in real time): read `seq_max` from the `/log` header,
then poll `/log?since=<seq_max>` repeatedly — each response carries the new high-water mark.
The ring holds the most recent 128 frames (whole messages, `FRAME_MAX`=512); if a poller falls
behind it auto-resyncs to the oldest retained frame.

## Remote reflash (OTA)

Device is in the wall (off USB). ArduinoOTA (espota) is compiled in but **can't be driven from
the WSL dev box** — WSL's NAT means the device can't connect back to it. So we use **HTTP-pull
OTA**: a tiny file server on **atlas** (`jordonsc@REDACTED-IP`, same LAN) hosts the firmware and
the device downloads it via `GET /update`. Proven working end-to-end.

Start the atlas server (`~/actron-ota/`, port 8088; survives SSH disconnect, not an atlas reboot):
```bash
ssh atlas 'setsid python3 -m http.server 8088 --directory $HOME/actron-ota >/tmp/actron-ota.log 2>&1 </dev/null &'
```
Reflash:
```bash
~/.pio-venv/bin/pio run -e um_tinyc6                                       # build
scp actron-sniffer/.pio/build/um_tinyc6/firmware.bin atlas:actron-ota/firmware.bin
curl "http://REDACTED-IP/update?url=http://REDACTED-IP:8088/firmware.bin"  # device pulls + reboots (~8s)
curl http://REDACTED-IP/stats        # confirm: the fw="<build date/time>" field changed
```
`/update` is unauthenticated (LAN-only experiment). From a **non-WSL** LAN host you can instead
push directly: `pio run -e um_tinyc6_ota -t upload`.

## RE workflow

1. Build the pass-through tap, meter-check 12V on pins 8/7, power the TinyC6 from USB-C.
2. `GET /measure` while triggering bus activity (change a zone setpoint in the Neo app).
3. `GET /set?baud=<inferred>` then try `parity=N` and `parity=E` (HVAC buses are often 8E1).
4. Correct baud/parity ⇒ stable repeating frames + low `rx_err` in `/stats`. Garbage +
   climbing `rx_err` ⇒ wrong; keep trying.
5. Poll `/log?since=` while performing **known actions** (toggle a specific zone, nudge one
   zone's setpoint, change mode) and diff frames to map fields — looking for per-zone
   setpoint/temperature bytes.

## Files

| File | Purpose |
|------|---------|
| `platformio.ini` | pioarduino platform (C6), board `um_tinyc6`, USB-CDC console |
| `src/main.cpp` | capture loop, ring buffer, WiFi + HTTP server, pulse-width baud estimator |
| `WIRING.md` | step-by-step T568B pass-through tap wiring guide |
| `FINDINGS.md` | **all research findings** — bus params, protocol structure, decoded fields, hardware topology; start here |
| `MAPPING-PLAN.md` | structured plan to map the full Actron functionality + path to control |
| `tools/decode.py` | Modbus decode tool: register snapshots, before/after diffs, CRC-16 — the mapping workhorse (`python3 tools/decode.py snapshot\|regs\|diff\|crc`) |
| `tools/findpulse.py` | Command-pulse finder: decodes the 0x66 *responses* and classifies each register as PULSE (command/event, e.g. reg14) / STEP (value) / drift. Used to map the command vocabulary — clear, make one NEO change, run it. **Set `gap=5000` first** (single-frame pulses get split at gap=3000). |
