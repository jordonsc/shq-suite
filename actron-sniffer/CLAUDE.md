# Actron RS485 Sniffer (experimental)

RS485 tool (sniffer **+ controller emulator + MITM bridge**) for the **Actron NEO ↔
indoor-unit RS485 bus** (the proprietary "IDU Interface"). Captures the Modbus traffic and
can either emulate a wall controller (0x67 pulse path — page-1 commands only) or sit as a
cut-bus MITM with two transceivers and rewrite the real NEO's response in flight (**covers
all command types including zone setpoints — now the recommended primary write path**).
Recon/prototype hardware, deployed in-wall.

It captures framed hex into a RAM ring buffer and serves it over an **open, LAN-only HTTP
server** so the capture can be observed and driven remotely (no USB babysitting). See
[`../docs/actron-local-control.md`](../docs/actron-local-control.md) for the wider
investigation.

**Status:** **deployed in the wall; full control map decoded.** Answer to the founding
question: **yes — the NEO puts per-zone setpoints, mode, fan, main setpoint, and a zone-enable
mask on the bus** (it's Modbus RTU 9600/8N1; values big-endian). The complete register map is in
[`FINDINGS.md`](FINDINGS.md) §7. Reachable at `http://REDACTED-IP/` (pinned) /
`redacted.local`. WiFi creds baked in (SHQ). `FRAME_MAX=512` (whole frames), OTA works.

**✅ Local write control achieved via two independent paths (FINDINGS §9).**

**Path A — 0x67 tap (2026-05-30):** emulate a secondary controller at 0x67 (answer its page-1
poll), send a command pulse — `reg 14 = <code>` for one response cycle with the new value in
the matching register. Proven for mode, fan, main setpoint. Non-invasive (NEO stays live).
Page-1 commands only.

**Path B — MITM bridge inject (2026-05-31):** cut the bus, run the firmware in `inject` mode,
fire `/pulse?rules=<reg>:<val>,...&n=2` for pulse-gated commands OR
`/inject?rules=<reg>:<val>,…` for persistent value-only writes. The bridge rewrites the real
NEO's page-1/page-2 response in flight and re-stamps CRC. Proven for **mode change
(heat → cool → off)**, **zone enable mask (Living+Entry → Jordon-only)**, and **zone cool
setpoints (Gym COOL across multiple values)** this session. **Recommended primary path** —
one mechanism covering every command type.

**See [`LOCAL-CONTROL-RECIPES.md`](LOCAL-CONTROL-RECIPES.md)** for the per-scenario recipes
(exact registers, values, additional context bytes). Quick reference of command codes:
**mode = 0x01, fan = 0x02, main setpoint = 0x04, zone enable = 0x40** (bitfield on reg 14).

Zone setpoints have **no command pulse** but DO require **reg 126.lo low nibble = mode bit**
(`0x01` cool, `0x02` heat) as a side-channel signal — without it, the board ignores the
INJECT. Discovered 2026-05-31 after extensive testing; this was the missing piece that made
INJECT-mode zone-setpoint writes finally work.

**Latency note (load-bearing for testing — FINDINGS §9):** the board accepts a pulse within
one cycle but takes **20–30 s** to publish the resulting state change in its broadcast (mode
changes are the slowest observed). Always wait ≥ 5 broadcast cycles before declaring a command
failed. The bridge's `B.mod` counter is a more reliable "pulse landed" signal than the
broadcast.

**✅ MITM bridge (deployed in-wall 2026-05-30, end-to-end proven):** Two UARTs —
**UART1 on GPIO16 (TX) / GPIO17 (RX) (indoor-board side)**, **UART0 on GPIO1 (TX) / GPIO0 (RX)
(NEO side; note the swapped RX/TX order vs UART1 — wiring convenience, C6 matrix is symmetric)** — sit between a physically
cut RS485 bus. Bytes are forwarded transparently in `passthru` mode; in `inject` mode the
firmware streams NEO-response bytes through `bridge::StreamingBridge`, substituting target
register values inline and re-stamping the Modbus CRC on the fly so the indoor board sees a
valid frame. This is the path for **per-zone setpoint writes** that 0x67/pulse can't reach.
The streaming-CRC injector is unit-tested on the host (`pio test -e native`, 21 cases). Bridge
and 0x67 emulator are **mutually exclusive** (both would drive UART1 TX) — switching to
non-OFF bridge auto-disarms `/armwrite`.

**Critical:** the byte-forwarding pump runs on a **dedicated FreeRTOS task** (`bridgeTask` in
`src/main.cpp`, priority 5) — not the Arduino main loop. Without this, HTTP / WiFi / OTA /
mDNS work on the main loop occasionally stalled byte forwarding for >3 ms mid-frame; the peer
TX FIFO would drain to empty during the stall and the destination saw a >t3.5 gap, treating
it as premature frame-end and rejecting the entire frame on CRC. Symptom in the capture was
frequent split frames (e.g. a 253-byte NEO response logged as 16+237 with a 6 ms gap). The
task busy-loops while either UART has bytes available (forwarding without preemption through
a full Modbus burst, max ~250 ms) and only yields via `vTaskDelay(1)` during the genuine
idle window between bursts. Don't move pumpCapture back to `loop()`.

**Open:** (a) command codes for **away / turbo / continuous-fan** still to map (page-1, quick
`findpulse.py` loop — likely on bits 3/4/5/7 of reg 14 by the bitfield pattern); (b) HA
integration — single endpoint per command using MITM `/bridge?mode=inject` + `/pulse`,
cloud demoted to telemetry. Firmware is receive-only unless `/armwrite` is called OR
`/bridge?mode=` is set to a non-OFF value (TX is firmware-gated). ~~hardware bench-test of
dual UART~~ done. ~~cut-bus tap + passthru~~ done. ~~zone-setpoint write~~ done. ~~mode / on-off /
zone-enable via MITM~~ done.

**Saved transition payloads in `captures/`:** four `neo_response_*.md` files each containing
the verified-first-transition page-1 + page-2 from a known user action (Living HEAT change,
Entry zone enable, Entry HEAT change, Living zone disable) plus a pair of random no-input
baselines. Useful as known-good replay templates and as ground truth for cross-comparison
analysis when mapping new commands.

**✅ `RESPOND` mode + `/loadtemplate` endpoint** (in `src/bridge.h`, `src/main.cpp`) — adds
a "block NEO entirely, replay a saved file payload to the board" capability. **End-to-end
proven (2026-05-31):** loaded the saved Living-HEAT-22.0 payload into the firmware via
`/loadtemplate?page=1|2&hex=…`, engaged `/bridge?mode=respond`, NEO went silent
(`B.frames=0` for 25 s), board adopted reg 135 = `00 DC` (22.0 °C) within ~3 s — exactly the
value-only commit the protocol model predicts. Pulse-gated registers in the payload (mode,
main setpoint, zone-enable mask) were correctly **not** adopted — confirming the model
holds: replayed value-only registers commit; replayed values for pulse-gated commands
don't, because the saved payload has reg 14 = 0 (no pulse). After exiting RESPOND, the NEO
resynced to the board's broadcast on the next polling cycle.

The historic OTA hangs that triggered ESP-IDF anti-brick rollback turned out to be
transient — not reproducible after USB-flash + immediate reboot. Likely network race during
the OTA download. If it happens again, force `g_bridge_mode = OFF` at the top of
`handleUpdate` before calling `httpUpdate.update()` so the UARTs go fully idle during the
flash window.

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

**Power the TinyC6 from a USB-C charger or power bank** (power only — the whole point is to
drop the data cable). Optionally run it from the bus 12V via a 12→5V buck into the 5V pin;
not needed for a bench experiment. **Do NOT power from a computer's USB port when running both
transceivers in the MITM bridge** — the combined draw of two RS485 drivers + WiFi can brown
out the regulator on a 500 mA-throttled host port, and the failure mode is sneaky: traffic
silently fails in one direction (the one whose transceiver tips the sag) while the other still
works. Verified 2026-05-30: full /loopback PASS on power bank, asymmetric silence on host USB.

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
| `GET /armwrite?addr=&ovr=reg:val,...&pulse=reg:val&pulsen=N&turn=<us>` | **ARM controller emulation (tap mode).** `addr` = slot to emulate (0x66/0x67/**0x68**; default 0x67; 0x66 needs the real NEO unplugged). `ovr` = persistent register overrides on the cached 0x66 template (big-endian). `pulse=reg:val` = one-shot command pulse applied to the next `pulsen` page-1 responses then reverts (e.g. `pulse=14:4` = setpoint command edge). `turn` = reply turnaround µs (default 5000, > t3.5 ≈3.65 ms). **Setpoint write:** `addr=0x67&ovr=12:220,56:220&pulse=14:4`. **Rejected (409) while `/bridge` mode != off**. |
| `GET /disarm` | stop **mutations**: 0x67 emulator off, INJECT bridge drops to PASSTHRU. The relay itself keeps running so a cut bus stays alive. OFF stays OFF (tap-mode case). |
| `GET /txprobe` | TX self-test on UART1: inject a poll to 0x66 and report whether it answers (rejected while bridge != off) |
| `GET /bridge?mode=off\|passthru\|inject` | **MITM bridge mode. Default on boot: PASSTHRU** (so a cut-bus deployment relays from the moment power is applied). OFF = capture only — DANGER if the bus is physically cut. PASSTHRU = forward both ways unchanged. INJECT = forward + apply `/inject` rules to the NEO→board direction. Switching to non-OFF force-disarms `/armwrite`. |
| `GET /inject?rules=reg:val,reg:val,...` | Set the substitution rules applied to NEO→board responses (`reg` = absolute Modbus register, `val` = big-endian 16-bit value). Up to 16 rules. Only effective when bridge mode is `inject`. |
| `GET /loopback?n=<bytes>` | **Dual-UART bench test.** Sends a deterministic pattern UART1→UART0 and UART0→UART1, reports byte integrity. With both transceivers wired in series via A↔A B↔B, expect 0 missing / 0 mismatched both ways at any n up to 1024. |
| `GET /uartcheck` | One-byte-per-direction probe for isolating "is UART0 alive at all?" — reports own-echo and cross-bus capture for each phase. Some auto-direction modules tri-state RO during DE, so 0-byte own-echo is not necessarily a fault; trust the cross-bus column. |
| `GET /blink` | Visual diagnostic: 6 bursts of 50 bytes (≈52 ms TX each, 500 ms gap) first on UART1 then UART0. Operator confirms which transceiver TX/RX LEDs light. |

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
| `platformio.ini` | pioarduino platform (C6), board `um_tinyc6`, USB-CDC console. Also defines `[env:native]` — host-only test runner (no board needed) for the bridge unit tests. |
| `src/main.cpp` | capture loop (dual-UART), ring buffer (interleaved A/B with source tag), 0x67 emulator, MITM bridge pump, WiFi + HTTP server, pulse-width baud estimator |
| `src/bridge.h` / `src/bridge.cpp` | Pure C++ (no Arduino deps) streaming MITM bridge — frame-aware byte forwarder with inline register substitution and on-the-fly CRC re-stamping. Unit-tested on the host. |
| `test/test_bridge/test_bridge.cpp` | Unity host-side tests for the bridge logic. 13 cases covering passthrough, injection, CRC re-stamping, multi-rule, boundary registers, mid-frame gap reset. Run with `pio test -e native`. |
| `WIRING.md` | step-by-step T568B pass-through tap wiring guide (tap-mode, single transceiver). The cut-bus MITM tap layout is documented in the FINDINGS / root project CLAUDE.md until it earns its own diagram. |
| `FINDINGS.md` | **all research findings** — bus params, protocol structure, decoded fields, hardware topology; start here |
| `MAPPING-PLAN.md` | structured plan to map the full Actron functionality + path to control |
| `tools/decode.py` | Modbus decode tool: register snapshots, before/after diffs, CRC-16 — the mapping workhorse (`python3 tools/decode.py snapshot\|regs\|diff\|crc`) |
| `tools/findpulse.py` | Command-pulse finder: decodes the 0x66 *responses* and classifies each register as PULSE (command/event, e.g. reg14) / STEP (value) / drift. Used to map the command vocabulary — clear, make one NEO change, run it. **Set `gap=5000` first** (single-frame pulses get split at gap=3000). |
