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
The streaming-CRC injector and the state decoder are unit-tested on the host
(`pio test -e native`, 38 cases — 21 bridge + 10 state + 7 mono). Bridge and 0x67 emulator are
**mutually exclusive** (both would drive UART1 TX) — switching to non-OFF bridge
auto-disarms `/armwrite`.

**Critical:** the byte-forwarding pump runs on a **dedicated FreeRTOS task** (`bridgeTask` in
`src/main.cpp`, priority 5) — not the Arduino main loop. Without this, HTTP / WiFi / OTA /
mDNS work on the main loop occasionally stalled byte forwarding for >3 ms mid-frame; the peer
TX FIFO would drain to empty during the stall and the destination saw a >t3.5 gap, treating
it as premature frame-end and rejecting the entire frame on CRC. Symptom in the capture was
frequent split frames (e.g. a 253-byte NEO response logged as 16+237 with a 6 ms gap). The
task busy-loops while either UART has bytes available (forwarding without preemption through
a full Modbus burst, max ~250 ms) and only yields via `vTaskDelay(1)` during the genuine
idle window between bursts. Don't move pumpCapture back to `loop()`.

**Yield budget (2026-07-19, ledger shq-suite-0019) — the "idle-only" yield above starved the
main loop under load.** The original code yielded *only* when BOTH UARTs were momentarily idle.
Under sustained bus traffic (heating season) that window rarely materialises, so the priority-5
`bridgeTask` busy-looped for **28–37 s** at a stretch and starved the priority-1 main loop —
which runs the HTTP server AND `ws_api::loop()`. Both went dark together while ICMP stayed up
and `seq_max` kept climbing (proved it was pure loop starvation, not WiFi), tripping HA's 30 s
availability timeout and flapping every `climate.actron_ac_*` entity unavailable ~hourly (the
user-facing symptom was `set_hvac_mode` failing with "no close frame received or sent" — a
command sent onto the already-silent socket). **Fix:** `bridgeTask` now caps the time it may
pump without yielding — `BRIDGE_YIELD_BUDGET_MS` (20 ms, preferred, taken between frames via
`isMidFrame()`), hard-capped at `BRIDGE_YIELD_HARD_MS` (250 ms ≈ one max Modbus frame). A single
`vTaskDelay(1)` is ~1 ms, **well under t3.5 (~3.65 ms)** — the same sub-threshold hiccup the
idle-yield already inserts — so relay integrity is preserved even if a forced yield lands
mid-frame (verified live: A/B frames keep flowing, `rxerr=0`). This makes the 07-13
`enableHeartbeat` (below) a belt-and-braces extra, not the actual cure — it addressed the wrong
layer (a starved WS loop can't service heartbeats anyway).

**⚠️ Regression #2 (2026-08-03, OPEN — ledger shq-suite-0019).** The yield-budget fix held only
07-20 → 07-24; from 2026-07-25 (~5.3 days after the 07-19 reboot) the HA `unavailable` flaps
returned at ~70–85/day with a **different signature**: ICMP + HTTP + `seq_max` all stay healthy
through a flap, but the ESP goes **silent on the one WS socket** — no data, no pong, no FIN
(HA only notices via its 20 s keepalive; atlas shows lingering FIN-WAIT-2 sockets because the
ESP never completes a close handshake). Somfy control group (same lib/heartbeat/AP/HA) is clean
→ device-side. Leading hypothesis: per-socket resource exhaustion with a ~5-day post-reboot
fuse (heap or lwIP PCB leak; each abandoned CLOSE_WAIT feeds the pressure). `/stats` now
carries telemetry for this: `heap`/`minheap`/`maxblk` (fragmentation shows as maxblk ≪ heap),
`uptime` (seconds, wraps with `millis()` at ~49.7 days), `ws` (current clients), and lifetime
counters `ws_conn`/`ws_disc`/`ws_err` — a growing `ws_conn − ws_disc` gap means sockets are
dying without the WS library ever seeing a DISCONNECTED event (the silent-death signature).
Fresh-boot baseline (fw "Aug 3 2026 10:54:22"): heap≈172k, maxblk≈152k.

**⇒ ROOT CAUSE FOUND (2026-08-19, ledger shq-suite-0034) — the exhaustion hypothesis above is
superseded.** The identical signature was caught in the act on a somfy-sdn controller and traced
to the app-level heartbeat, which both firmwares share: `millis()` on these C6 boards occasionally
returns a value far in the future (proved by error-ring entries stamped *beyond* the device's own
uptime), and the periodic push was gated on the **signed** form
`(int32_t)(t - last_heartbeat_ms_) >= (int32_t)HEARTBEAT_INTERVAL_MS`, which reads "not due yet"
for as long as the stamp sits ahead of the clock. The socket, the WS pings and the connect-time
snapshot all keep working — which is exactly why ICMP/HTTP/`seq_max` looked healthy through every
flap. Fixed on both firmwares (actron build "Aug 18 2026 23:07:14"): the gate is now an
**unsigned** elapsed test that fires on the next loop instead of wedging, and `mono::now()`
(`src/mono.{h,cpp}`, twin of somfy-sdn's) filters the bad reads out at source. `/stats` gained
`hb_age`/`hb_tx` (heartbeat still running?) and `clk_torn`/`clk_back`/`clk_jump`/`clk_jumpms`
(how often the clock misbehaves). If the flaps return with `hb_age` small and `hb_tx` still
climbing, it is a *different* fault — go back to the exhaustion hypothesis then, not before.

**⇒ FLAPS RETURNED 2026-08-20, DIFFERENT FAULT AGAIN (ledger shq-suite-0038) — and this time the
device instruments itself.** Exactly the condition the note above told a future session to watch
for: `hb_age` small, `hb_tx` climbing, `clk_*` flat zero, uptime unbroken, and 154 HA `unavailable`
events in 35 h anyway (median 23 s, 4-8/hour) after 48 completely clean hours post-flash. Two
measurements narrowed it before any code changed: 40 min of 1 Hz ICMP returned **2397/2398** replies
from the device against **2344/2344** from the gateway (so the radio and IP stack never left —
association/rekey/power-save theories are dead), while plain `GET /stats` **timed out at 5 s on 16
of 151 requests**. The device answers ICMP and stalls TCP. Polling `/stats` every 15 s **quadrupled
the flap rate** (22 events in 40 min vs 4-8/hour) and drew free heap down 172 k → 148 k, recovering
once polling stopped — HTTP requests starve the WS service, which is both a finding and an
on-demand reproducer. `ss -tn` on atlas showed exactly one socket to `:8767`, so it is not an
HA-side coordinator leak.

**Rather than keep guessing, the firmware now records why.** `src/diag.{h,cpp}` classifies every WS
disconnect from evidence held at the instant it fires and stamps it with the machine's condition —
see "Self-diagnostics" below. The two live hypotheses it exists to separate:
* **`pong_timeout` with a large `loop_max_ms`** ⇒ the main loop stalls past the library's ping/pong
  deadline and `enableHeartbeat` evicts a perfectly healthy HA. Check `http_ms` — Arduino's
  `WebServer::handleClient()` blocks the whole loop while a slow request dawdles.
* **any disconnect with `sock=0`** ⇒ the lwIP socket pool is exhausted. HTTP and WS share it.

**⇒ ROOT CAUSE FOUND AND FIXED (2026-08-24, ledger shq-suite-0038).** 20.3 h of self-recorded
diagnostics answered it: 149 loop stalls, worst **60,069 ms with 60,068 ms in the `ws_api::loop()`
phase** (`http_max=9ms`, `ota_max=3ms` over the whole window — the HTTP and socket-pool hypotheses
are both dead: `sock=3` on every disconnect record, heap flat, `wifi_disc=0`, `clk_*=0`).
arduinoWebSockets does **blocking socket I/O bounded by `WEBSOCKETS_TCP_TIMEOUT` (default
5000 ms)** — hand-rolled spin loops in `WebSockets.cpp` — so one zombie client cost the main loop
5 s per operation, stalls compounded across the 5 client slots (observed stalls were clean
multiples: 10 s / 20 s / 60 s), late pongs made `enableHeartbeat`'s reaper evict the *healthy* HA
client, and each reconnect fed the loop (five slots seen dying in the same second). The fix, on
both twins (actron build "Aug 24 2026 00:39:51", somfy fw 1.6.1):
* **`-D WEBSOCKETS_TCP_TIMEOUT=500`** (`platformio.ini`) — bounds the worst per-client stall 10×,
  keeping even a pathological loop pass under the 5 s pong deadline. This is the load-bearing
  stall fix; somfy already had the dirty-flag architecture and still hit a 50 s stall without it.
* **Dirty-flag broadcast port** (somfy's pattern): `ws_api::publishStateIfChanged` (called from
  the priority-5 bridge task) is gone; `notifyStateChanged` copies a `ControllerState` snapshot
  under a `portMUX` spinlock and sets a flag, and `ws_api::loop()` (main loop) drains, diffs and
  broadcasts. The bridge task never touches a socket again — previously a zombie client could
  stall the **RS485 relay itself** for seconds mid-broadcast, and the unsynchronised cross-task
  use of the WS library (no internal locking) was a data race in its own right.
* **`WEBSOCKETS_SERVER_CLIENT_MAX` deliberately stays at the library default 5** — with the 500 ms
  bound the slot count no longer matters for the feedback loop, and lowering it interacts badly
  with the wedge watchdog (5 min continuously at cap ⇒ reboot: a small cap turns "HA + a couple of
  debug clients" into a self-reboot) and refuses debug connections during zombie windows.
* **`diag::tick()` wrap guard**: an `hb_age` ≥ 0x80000000 is a wrapped "negative" (future stamp),
  clamped to 0 so it can never latch a bogus `heartbeat_stall` record (the ~4.29e9 values in the
  0038 data). The race that produced them died with the port (single writer), guard kept anyway.

**⇒ SECOND MECHANISM, FOUND AND FIXED 2026-08-27 (build "Aug 27 2026").** The 72 h read on the
fix above was a large partial win, not a cure: the quiet majority of the fleet dropped to a
806-1629 ms worst loop stall (2-3 x the new 500 ms bound, exactly as designed) and the actron ran
**63 hours completely clean** — then turned, with a residual **50,059 ms** stall. The same figure
appeared on three somfy controllers within a 4 ms spread (50,056 / 50,057 / 50,058), which is one
deterministic timeout, not an accumulation.

It is in the **Arduino core**, not lwIP and not the WS library. `NetworkClient::write()` retries up
to `WIFI_CLIENT_MAX_WRITE_RETRY` (10) times around a `select()` bounded by
`WIFI_CLIENT_SELECT_TIMEOUT_US` (1 s) — so one write to a peer that stopped reading blocks **~10 s**.
`WEBSOCKETS_TCP_TIMEOUT` only gets re-checked *between* write calls, and `SO_SNDTIMEO` is a red
herring (the core's `send()` already uses `MSG_DONTWAIT`; the blocking is in the `select()`). Both
constants are unguarded `#define`s, so no build flag reaches them, and patching the framework would
be a machine-global change that a toolchain update silently reverts. `broadcastTXT()` walks every
slot at a header + payload write each, so N dead slots multiply the 10 s quantum — which also
re-reads the earlier numbers correctly: 60,069 / 50,05x / 20,024 / 10,015 ms are all clean multiples
of **10 s**, not of the 5 s library timeout as first recorded.

Fix: **`src/ws_guard.{h,cpp}`** (twin of somfy's), a `GuardedWebSocketsServer` subclass — the
library's `_clients[]` is `protected`, so this needs no library patch. It polls each socket with a
**zero-timeout `select()`** before writing, skips any that would block (`broadcastWritableTXT()`
replaces `broadcastTXT()` at every site), and drops a socket that stays unwritable for
`WS_STALL_REAP_MS` (10 s — under HA's 30 s availability timeout so the reconnect lands in time,
well over any transient full send buffer). The reap closes the socket directly rather than sending a
WS close frame, because that frame would be a write to the very socket that is refusing writes. New
`ws_stall_reap` diag event + `stall_reaps` counter make the fix measurable: every reap is a ~10 s
stall that did not happen.

**⇒ fw generation "Aug 28 2026" — the guard closed, plus BSSID reporting.** The 1.7.x canary read
was decisive both ways: the actron hit a **17 ms** worst loop stall with zero flaps over 16.6 h
(against 50,059 ms and 4-8/hour), but the somfy twin still logged **10,016 ms** — one 10 s quantum
where it had been five. Two holes in the guard explained the remainder, both now closed:
* **`reapStalled()` now runs BEFORE `server.loop()`.** `enableHeartbeat`'s ping is emitted from
  inside `server.loop()` and writes to the socket **directly**, bypassing `broadcastWritableTXT()`.
  Reaping afterwards let the library ping a blocked socket first — the full ~10 s core write.
* **`WS_STALL_REAP_MS` 10 s → 3 s**, under the 15 s ping interval so a dead socket is reaped
  several times over before the library can touch it, still far above any transient full buffer.
* **`sendWritableTXT()`** now guards the per-client paths too (connect-time snapshot, acks, and the
  diag backlog — the largest frame emitted, sent to a client of unknown socket health).

**Station-side BSSID reporting** landed in the same build: `bssid=`/`roams=` in `/stats` and
`/diag`, `bssid`/`wifi_roams`/`skipped_writes` in the health push (HA sensors in
`actron_mitm_controller` 1.4.0), plus an `ap_change` diag event. It exists to settle "device fault
or AP fault?", and it did — **the answer was no AP clustering** (ledger shq-suite-0040). Read
association from the **station**, never the UniFi controller client list (wiki
`estate/shq-network.md` records the controller disagreeing with the station).

**⚠️ Build gotcha:** `app_desc.cpp` keeps a STALE `__DATE__` across rebuilds — PlatformIO caches
object files by content hash, so an unchanged `app_desc.cpp` reports the old date in the image
descriptor while `main.cpp`'s `fw=` string refreshes normally. Touch that file when you want the
descriptor to agree; a note in it says so.

## Self-diagnostics (`src/diag.{h,cpp}`, 2026-08-23)

Twin of `somfy-sdn/src/diag.{h,cpp}` (ported there in somfy fw 1.6.0) — **keep the two in step**,
same standing rule as `mono.{h,cpp}`.

Both twins carry the `clk_back` fix (`clockGlitchTotal()` counts torn + jump only, never
`mono::backwardReads()` — ledger shq-suite-0039); it is inert here (`bridgeTask` never calls
`mono::now()`, so `clk_back` stays 0) and load-bearing on somfy, where the bus task drives it to
~72/s. Source and flashed binary are in step as of 2026-08-24 00:39 (the shq-suite-0038 fix
build — dirty-flag port + `WEBSOCKETS_TCP_TIMEOUT=500` + diag wrap guard).

A 48-entry RAM ring of `Record`s (~3 kB held permanently). Each carries the event, an inferred
reason, and the machine's condition at capture: free heap, largest allocatable block, spare lwIP
sockets, worst main-loop and `handleClient()` stall since the previous record, RSSI, client count.

**Events:** `boot` (with `esp_reset_reason()`), `ws_connect`, `ws_disconnect`, `ws_error`,
`ws_at_cap`, `loop_stall`, `heap_low`, `socket_low`, `wifi_down`/`wifi_up`, `clock_glitch`,
`heartbeat_stall`.

**Disconnect reasons**, inferred conservatively — `unclassified` is preferred to a confident wrong
answer, and the raw evidence rides along so a verdict can be re-judged from history without a
reflash:
| Reason | Inferred when |
|--------|---------------|
| `pong_timeout` | last pong ≥ 20 s old (ping 15 s + pong 5 s) ⇒ **our own reaper** evicted it |
| `peer_close` | a pong landed within the last ping cycle, or inbound traffic within 10 s ⇒ not us |
| `transport_error` | a `WStype_ERROR` arrived for that slot in the preceding 2 s |
| `unclassified` | quiet socket, pong not yet overdue — nothing to pin it on |

The WS library gives no callback for a received close frame, so `peer_close` is inferred rather
than observed; HA's own `ha_clean`/`ha_closed` bus event is the confirming half.

**Spare sockets** are measured, not guessed: `probeSockets()` asks lwIP for sockets until it
refuses (up to 3) and closes them again. `sock=0` is the direct test of pool exhaustion. Re-probed
at the instant of every disconnect, not reused from the 15 s sample.

**Loop phase timing.** `loop()` times `ArduinoOTA.handle()`, `server.handleClient()` and
`ws_api::loop()` separately and feeds `diag::noteLoop()`. A `loop_stall` record therefore says
*which* phase held the loop — an HTTP problem, an OTA problem, and a WS problem need different
fixes. `diag::tick()` is called every iteration but rate-limits its own body to 1 Hz:
`ESP.getFreeHeap()` takes a heap lock and running it at loop rate would perturb the subsystem
under investigation.

**Delivery — three surfaces, and the backlog is the load-bearing one:**
1. Live over WS as `{"type":"diag","event":{…}}`, **polled** from `ws_api::loop()` (max 4/iteration)
   rather than pushed from a callback — emitting a frame from inside the library's own event
   dispatch would re-enter the server mid-iteration.
2. `{"type":"diag_backlog","events":[…]}` — the last 12 records, replayed to every client the
   moment it connects. **A socket can never be told about its own death**, so this is the only way
   HA ever learns why the previous session ended. Records are deliberately re-sent (the live
   cursor is not advanced past a backlog) — HA de-duplicates on `seq`.
3. `{"type":"health","data":{…}}` every 30 s, plus `GET /diag` (text) and `GET /diag.json`.

`/stats` gained a summary line: `sock` `loop_max` `http_max` `stalls` `pongto` `peerclose` `txerr`
`wifi_disc` `diag_seq`.

**Open:** command codes for **away / turbo / continuous-fan** still to map (page-1, quick
`findpulse.py` loop — likely on bits 3/4/5/7 of reg 14 by the bitfield pattern). Firmware
is receive-only unless `/armwrite` is called OR `/bridge?mode=` is set to a non-OFF value
(TX is firmware-gated). ~~hardware bench-test of dual UART~~ done. ~~cut-bus tap + passthru~~ done.
~~zone-setpoint write~~ done. ~~mode / on-off / zone-enable via MITM~~ done.
~~HA integration~~ done — see "Controller API" below.

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

**OTA hardening (2026-05-31)** — root cause of the historic OTA hangs identified and
fixed. `bridgeTask` runs at priority 5; the priority-1 main loop is where `httpUpdate`
lives. Two related bugs combined to starve the download whenever the RS485 bus was
active:

1. `bridgeTask` only called `vTaskDelay(1)` when BOTH UARTs were idle. With
   `g_capture=false` (set by `handleUpdate`), `pumpCapture()` was skipped — but
   `pumpCapture` is the only thing that drains the UART RX FIFOs. So as soon as one byte
   arrived, `available()` stayed sticky-true and the task busy-spun without yielding,
   starving the main loop entirely.
2. `handleUpdate` set `g_capture=false` but didn't suspend the bridge task, so the
   busy-spin above kicked in immediately.

Fix in `bridgeTask`: unconditionally `vTaskDelay(1)` when `g_capture=false`. Fix in
`handleUpdate`: `vTaskSuspend(g_bridge_task)` + drain UART FIFOs + force bridge mode
OFF before calling `httpUpdate.update()`. This was previously masked because PASSTHRU's
per-byte cost was tiny; flipping the boot default to INJECT (CRC tracking work per byte)
made the starvation reproducible. Validated end-to-end: OTA under heavy bus traffic
(B.frames=100 in flight) completed cleanly with a fresh `fw=` timestamp post-flash.

Build cache gotcha: PlatformIO uses content-hash caching for `__DATE__`/`__TIME__`
embedding. `touch src/main.cpp` is NOT enough to bump the `fw=` string in `/stats` —
you have to change the file's content (even a comment) before a rebuild will refresh
those macros.

**Device identity & OTA app-guard (2026-06-26).** This board shares the `40:4c:ca:51`
OUI and the `POST /update` endpoint with the somfy-sdn controllers, so the image carries
a native app id and `/update` refuses a foreign one:
- `src/app_desc.cpp` overrides `esp_app_desc.project_name` = `"actron-mitm"` (strong
  `extern "C"` symbol in `.rodata_desc` shadows the prebuilt `arduino-lib-builder` copy).
  Twin of `somfy-sdn/src/app_desc.cpp`.
- `/stats` starts `# app=actron-mitm`; `/stats.json` exposes `app`/`mac`.
- `handleUpdate` runs `httpUpdate` with `rebootOnUpdate(false)`, then checks the written
  boot partition's `esp_app_desc.project_name`; if it isn't `"actron-mitm"` it reverts the
  boot partition and does **not** reboot. The guard's twin was validated live on the somfy
  Jordon Study canary (an actron image was correctly rejected). **Not yet flashed to the
  in-wall Actron** — its guard/id only take effect after the next OTA. See
  `somfy-sdn/CLAUDE.md` → "Device identity & OTA app-guard" and ledger shq-suite-0012.

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

## WiFi provisioning (`src/wifi_prov.{h,cpp}`)

**No baked credentials** — SSID/password live in NVS (Arduino `Preferences`, namespace `actron`),
provisioned at runtime via a SoftAP captive portal. Ported from `somfy-sdn/src/wifi_prov.{h,cpp}`
(trimmed: no motor/bus logic). Boot flow: read NVS → STA-connect with retries (all-channel scan,
strongest AP) → on absence/failure start the SoftAP **`actron-mitm-XXXX`** (XXXX = last 2 STA-MAC
octets) serving a WiFi-setup form. Provisioned creds survive OTA. Hostname/AP = `actron-mitm-XXXX`.

- **The RS485 bridge runs in BOTH modes** (it's on its own FreeRTOS task), so the A/C keeps
  bridging even while the controller sits unprovisioned in the portal. The app HTTP/WS servers
  only start once STA-connected (`startAppServer()`); in portal mode `wifi_prov` owns port 80.
- **Re-provision / move networks:** `POST /wifireset` wipes creds and reboots into the portal.
- **⚠️ NO GPIO0 button here** (the somfy port has one): **GPIO0 is the NEO-side UART0 RX**
  (`PIN_B_RX`). `pinMode(GPIO0, …)` kills NEO reception (`B.frames=0`, no A/C state to decode, HA
  shows wrong status). This bit us during the port (2026-06-26, ledger shq-suite-0013) — never
  touch GPIO0/GPIO1 from non-UART code.

Replaces the old baked-`secrets.h` creds, which knocked the in-wall device off the network when an
image was built without `secrets.h` and left no remote recovery (ledger shq-suite-0013). With
provisioning, a failed connect falls back to the device's own AP — self-recoverable, no USB.

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
| `GET /stats` | one-line status; starts `# app=actron-mitm …`. Carries `heap/minheap/maxblk/uptime`, `ws/ws_conn/ws_disc/ws_err`, and (2026-08-19) `hb_age`/`hb_tx` + `clk_torn`/`clk_back`/`clk_jump`/`clk_jumpms`, and (2026-08-23) `sock`/`loop_max`/`http_max`/`stalls`/`pongto`/`peerclose`/`txerr`/`wifi_disc`/`diag_seq` |
| `GET /diag` | **self-diagnostics** — health line + the event ring, newest last. Reachable when the WS layer is precisely what has stopped working |
| `GET /diag.json` | same, machine-readable (`{"health":{…},"events":[…]}`) |
| `GET /stats.json` | machine-readable identity + status (`app`/`model`/`mac`/`ip`/`fw`/`rssi`/`bridge`) — uniform with somfy-sdn so any TinyC6 on the LAN is positively identifiable |
| `GET /log?since=<seq>&n=<max>` | frames; `since` returns only newer ones (incremental polling) |
| `POST /set?baud=&parity=N\|E\|O&gap=<us>` | change capture settings live |
| `GET /measure` | estimate baud from raw line pulse widths (~5s; needs bus activity) |
| `POST /clear` | reset ring + counters |
| `POST /update?url=<bin>` | **HTTP-pull OTA** — download firmware from a URL and self-flash (see Remote reflash) |
| `POST /armwrite?addr=&ovr=reg:val,...&pulse=reg:val&pulsen=N&turn=<us>` | **ARM controller emulation (tap mode).** `addr` = slot to emulate (0x66/0x67/**0x68**; default 0x67; 0x66 needs the real NEO unplugged). `ovr` = persistent register overrides on the cached 0x66 template (big-endian). `pulse=reg:val` = one-shot command pulse applied to the next `pulsen` page-1 responses then reverts (e.g. `pulse=14:4` = setpoint command edge). `turn` = reply turnaround µs (default 5000, > t3.5 ≈3.65 ms). **Setpoint write:** `addr=0x67&ovr=12:220,56:220&pulse=14:4`. **Rejected (409) while `/bridge` mode != off**. |
| `POST /disarm` | stop **mutations**: 0x67 emulator off, INJECT bridge drops to PASSTHRU. The relay itself keeps running so a cut bus stays alive. OFF stays OFF (tap-mode case). |
| `POST /txprobe` | TX self-test on UART1: inject a poll to 0x66 and report whether it answers (rejected while bridge != off) |
| `POST /bridge?mode=off\|passthru\|inject` | **MITM bridge mode. Default on boot: INJECT** — so the WS Controller API can issue writes without an HTTP poke first. With zero rules INJECT is functionally equivalent to PASSTHRU (CRC tracking adds ~166 ns/byte vs. a 1.04 ms byte window at 9600 baud — invisible). OFF = capture only — DANGER if the bus is physically cut. PASSTHRU = forward both ways unchanged. Switching to non-OFF force-disarms `/armwrite`. |
| `POST /inject?rules=reg:val,reg:val,...` | Set the substitution rules applied to NEO→board responses (`reg` = absolute Modbus register, `val` = big-endian 16-bit value). Up to 16 rules. Only effective when bridge mode is `inject`. |
| `POST /loopback?n=<bytes>` | **Dual-UART bench test.** Sends a deterministic pattern UART1→UART0 and UART0→UART1, reports byte integrity. With both transceivers wired in series via A↔A B↔B, expect 0 missing / 0 mismatched both ways at any n up to 1024. |
| `POST /uartcheck` | One-byte-per-direction probe for isolating "is UART0 alive at all?" — reports own-echo and cross-bus capture for each phase. Some auto-direction modules tri-state RO during DE, so 0-byte own-echo is not necessarily a fault; trust the cross-bus column. |
| `POST /blink` | Visual diagnostic: 6 bursts of 50 bytes (≈52 ms TX each, 500 ms gap) first on UART1 then UART0. Operator confirms which transceiver TX/RX LEDs light. |

Frame line: `<seq> <t_s> +<gap>us <len>: HEX...  |ascii|`

**Incremental polling** (how to watch in real time): read `seq_max` from the `/log` header,
then poll `/log?since=<seq_max>` repeatedly — each response carries the new high-water mark.
The ring holds the most recent 128 frames (whole messages, `FRAME_MAX`=512); if a poller falls
behind it auto-resyncs to the oldest retained frame.

## Controller API (WebSockets, port 8767)

Push-based client API consumed by the `actron_mitm_controller` Home Assistant integration.
The HTTP API above is for reverse-engineering / operator control; this is the runtime
surface for HA. See `src/ws_api.cpp` for the implementation.

Connect to `ws://REDACTED-IP:8767/` — every client is auto-subscribed on connect; a full
`state` snapshot is sent immediately, then on every state change, plus a 10 s heartbeat.

Outgoing messages:
- `{"type":"state","data":{...}}` — full snapshot (mode/fan/master_setpoint/**current_temp**
  + zones[8] each with current_temp + target_temp + enabled). The master's `current_temp` is
  the indoor unit's main/return-air reading (reg 13 — board-aggregated across enabled zones,
  matches what the NEO shows as the system current temp); read-only, no transition sibling.
  For every controllable field there's a sibling `<field>_transitioning` that holds the
  pending target value while a write is in flight (cleared on board adoption, or on give-up —
  see the write-reliability note below).
- `{"type":"ack","id":"<cmd_id>","status":"accepted"}` / `{"type":"error","id":"<cmd_id>",
  "message":"..."}` — replies to a command, correlated by the client-supplied `id`.

Incoming commands (all with optional `id` for ack matching):
- `set_mode` — `value` in `off`/`cool`/`heat`/`auto`/`fan`
- `set_fan` — `value` in `low`/`med`/`high`/`auto`
- `set_master_setpoint` — `value` in °C (10..35, 0.1 step)
- `set_zone_enabled` — `zone` 0..7, `value` bool
- `set_zone_setpoint` — `zone` 0..7, `value` in °C

The server keeps the bridge in INJECT mode permanently. Writes use the recipes from
`LOCAL-CONTROL-RECIPES.md`. Pulse-style writes (mode/fan/master setpoint/zone enable)
arm a 2-frame pulse in `StreamingBridge::setPulse` and auto-expire. Zone setpoint writes
arm persistent INJECT rules including the reg 126 commit-signal nibble; ws_api clears
them on board adoption or grace timeout. In AUTO master mode, zone setpoints commit in
two phases (cool array first then heat array) so both stores stay in sync.

**Write reliability — retry vs. hold (FINDINGS-driven, see HA history analysis).** A landed
pulse is adopted + re-broadcast by the board within ~3–6 s, consistently; a transition still
unadopted after ~10 s means that 2-frame pulse was *dropped* (transient bus error), not slow.
The two write classes are therefore handled differently in `tickTransitions`:
- **Pulse commands** (mode/fan/master setpoint/zone enable): `armPulse` stores the rules and
  fires; `tickPulse` re-fires every `PULSE_RETRY_INTERVAL_MS` (10 s) up to `PULSE_MAX_FIRES`
  (6 total attempts) until adopted, then gives up (~60 s effective window). Re-firing is
  idempotent (same target value/command code).
- **Zone setpoints** (persistent INJECT): NOT retried — the rule is already held on *every*
  response frame until adoption, so the continuous hold *is* the retry. They just wait one
  `ZONE_SETPOINT_GRACE_MS` (60 s) window **per commit phase** before bailing (AUTO = up to 2×).

**Concurrent-change semantics: latest-wins.** When two commands arrive on the same field
within the transition window, the second overwrites the first (target value, deadline,
and bridge rules all replace). One subtle bug to remember: `cmdSetZoneEnabled` builds
the new mask by layering on top of `ze_t_.target_raw` if a transition is already active,
NOT `current_state_.zone_enable_mask`. Without that, rapid consecutive enable commands
silently drop the earlier bit changes (each command bases its mask on the stale bus
state which hasn't yet seen the first commit). All other commands are naturally
latest-wins because they replace a single target value rather than layering.

**Important: the indoor board silently rejects writes to unconfigured zone slots** (e.g.
zones 6/7 in a 6-zone system). The bridge dutifully injects the write — `B.mod` even
increments — but the board's broadcast never reflects the change. Use real zones for
write tests, or expose this as an error in a future revision.

The HTTP API stays usable for RE work — it doesn't affect bridge state or the WS layer.

## Remote reflash (OTA)

Device is in the wall (off USB). ArduinoOTA (espota) is compiled in but **can't be driven from
the WSL dev box** — WSL's NAT means the device can't connect back to it. So we use **HTTP-pull
OTA**: a tiny file server on **atlas** (`jordonsc@REDACTED-IP`, same LAN) hosts the firmware and
the device downloads it via `POST /update`. Proven working end-to-end.

Start the atlas server (`~/actron-ota/`, port 8088; survives SSH disconnect, not an atlas reboot):
```bash
ssh atlas 'setsid python3 -m http.server 8088 --directory $HOME/actron-ota >/tmp/actron-ota.log 2>&1 </dev/null &'
```
Reflash:
```bash
~/.pio-venv/bin/pio run -e um_tinyc6                                       # build
scp actron-sniffer/.pio/build/um_tinyc6/firmware.bin atlas:actron-ota/firmware.bin
curl -X POST "http://REDACTED-IP/update?url=http://REDACTED-IP:8088/firmware.bin"  # device pulls + reboots (~8s)
curl http://REDACTED-IP/stats        # confirm: the fw="<build date/time>" field changed
```
`/update` (and all mutating endpoints) are **POST** — `curl -X POST` (query args still parse).
It is unauthenticated (LAN-only experiment). From a **non-WSL** LAN host you can instead
push directly: `pio run -e um_tinyc6_ota -t upload`.

## RE workflow

1. Build the pass-through tap, meter-check 12V on pins 8/7, power the TinyC6 from USB-C.
2. `GET /measure` while triggering bus activity (change a zone setpoint in the Neo app).
3. `POST /set?baud=<inferred>` then try `parity=N` and `parity=E` (HVAC buses are often 8E1).
4. Correct baud/parity ⇒ stable repeating frames + low `rx_err` in `/stats`. Garbage +
   climbing `rx_err` ⇒ wrong; keep trying.
5. Poll `/log?since=` while performing **known actions** (toggle a specific zone, nudge one
   zone's setpoint, change mode) and diff frames to map fields — looking for per-zone
   setpoint/temperature bytes.

## Files

| File | Purpose |
|------|---------|
| `platformio.ini` | pioarduino platform (C6), board `um_tinyc6`, USB-CDC console; lib_deps for `Links2004/WebSockets` (Controller WS API) and `bblanchon/ArduinoJson` (command/state serialisation). Also defines `[env:native]` — host-only test runner for the bridge, state decoder + clock filter. |
| `src/main.cpp` | capture loop (dual-UART), ring buffer (interleaved A/B with source tag), 0x67 emulator, MITM bridge pump, WiFi + HTTP server, pulse-width baud estimator. Wires `state::feedNeoFrame` into the B-side frame-complete path and `ws_api::loop()` into the Arduino main loop. |
| `src/bridge.h` / `src/bridge.cpp` | Pure C++ (no Arduino deps) streaming MITM bridge — frame-aware byte forwarder with inline register substitution and on-the-fly CRC re-stamping. Unit-tested on the host. |
| `src/state.h` / `src/state.cpp` | Pure C++ NEO frame decoder — parses page-1/page-2 func-03 responses into a `ControllerState` struct (mode/fan/setpoints/zones/temps) per FINDINGS §7. Used by ws_api to publish state and tick transitions. Host-testable. |
| `src/ws_api.h` / `src/ws_api.cpp` | WebSockets server on port 8767 — JSON command/state schema, transition table with per-field `*_transitioning` values, write orchestration mapping each command to the LOCAL-CONTROL-RECIPES recipes. Bridge stays in INJECT permanently; pulse rules auto-expire, persistent rules (zone setpoints) are cleared on board adoption or `GRACE_PERIOD_MS` timeout. **WS liveness (2026-07-13):** `begin()` calls `enableHeartbeat(15000,5000,2)` to evict half-open zombie clients (a WiFi blip leaving a client's TCP half-open, no FIN). **This was NOT the actual cure for the recurring ~30 s `unavailable` flaps** — root cause (found 2026-07-19) is `bridgeTask` loop-starvation, fixed by the yield budget in `main.cpp` (see the "Yield budget" note up top); the heartbeat can't help when the WS loop that *sends* it is the starved thing. Kept as belt-and-braces for genuine half-open clients. Plus a **wedge-watchdog**: `loop()` reboots if all `WEBSOCKETS_SERVER_CLIENT_MAX` (5) slots stay occupied continuously for 5 min (backstop to the client-side leak fixed in `actron_mitm_controller` 1.1.1; any drop below the cap resets the timer, so it can't boot-loop). Both ported from `somfy-sdn/src/ws_api.cpp` (fw 1.1.5 + 1.3.0). See ledger shq-suite-0019. **Heartbeat-wedge fix (2026-08-19, ledger shq-suite-0034):** the periodic push is now gated on an *unsigned* elapsed test and `now_ms()` returns `mono::now()` — see the Regression #2 note up top for why the signed form wedged for hours. `hb_age`/`hb_tx` in `/stats` say whether the push is alive. |
| `src/diag.h` / `src/diag.cpp` | **Self-diagnostics** (ledger shq-suite-0038; twin ported to somfy-sdn fw 1.6.0) — WS disconnect classification with the machine's condition attached, loop/HTTP phase-stall timing, an lwIP spare-socket probe, and a 48-entry event ring served at `/diag`, `/diag.json`, and pushed over WS (live + replayed as a backlog on connect). Twin of `somfy-sdn/src/diag.{h,cpp}` — keep them in step; note the source-ahead-of-binary warning in that section. |
| `src/mono.h` / `src/mono.cpp` | Pure C++ glitch-filtered monotonic clock (twin of `somfy-sdn/src/mono.{h,cpp}` — keep them in step). `mono::now()` backs `ws_api`'s `now_ms()`, so every transition deadline and the heartbeat stamp are immune to a single far-future `millis()` read. Host-tested. |
| `test/test_mono/test_mono.cpp` | Unity host-side tests for the clock filter. 7 cases: normal progression, a far-future glitch in either sample, backwards reads, a genuine long stall being accepted, torn tolerance, 49.7-day wrap. |
| `test/test_bridge/test_bridge.cpp` | Unity host-side tests for the bridge logic. 21 cases covering passthrough, injection, CRC re-stamping, multi-rule, boundary registers, mid-frame gap reset, pulse expiry, replay templates. Run with `pio test -e native`. |
| `test/test_state/test_state.cpp` | Unity host-side tests for the NEO state decoder. 10 cases covering frame validation (CRC / addr / func / bytecount), page-1 + page-2 field decode, zone enable mask change, active-array setpoint helper, mode/fan name roundtrip. Run with `pio test -e native`. |
| `WIRING.md` | step-by-step T568B pass-through tap wiring guide (tap-mode, single transceiver). The cut-bus MITM tap layout is documented in the FINDINGS / root project CLAUDE.md until it earns its own diagram. |
| `FINDINGS.md` | **all research findings** — bus params, protocol structure, decoded fields, hardware topology; start here |
| `MAPPING-PLAN.md` | structured plan to map the full Actron functionality + path to control |
| `tools/decode.py` | Modbus decode tool: register snapshots, before/after diffs, CRC-16 — the mapping workhorse (`python3 tools/decode.py snapshot\|regs\|diff\|crc`) |
| `tools/findpulse.py` | Command-pulse finder: decodes the 0x66 *responses* and classifies each register as PULSE (command/event, e.g. reg14) / STEP (value) / drift. Used to map the command vocabulary — clear, make one NEO change, run it. **Set `gap=5000` first** (single-frame pulses get split at gap=3000). |
