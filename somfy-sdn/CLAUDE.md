# Somfy SDN controller

ESP32-C6 (Unexpected Maker **TinyC6**) controller for **Somfy Digital Network (SDN)** RS485
blind motors. A **normal bus participant** (single auto-direction transceiver tapped in parallel
across A/B — **not** a MITM, unlike `actron-sniffer`). TX is firmware-gated by a LISTEN/ACTIVE
mode, but boots **ACTIVE** with an auto-discovery sweep (as-built deviation from SPEC §6.2's
conservative LISTEN default — chosen so HA covers work autonomously after a reboot on this
single-controller bus; see "Bus mode & discovery" below). Replaces the Matter path the `omni` app
provided in the separate `matter-apps` repo (`common/features/app_sdn.cpp`).

Design twin of `actron-sniffer`: same firmware stack (PlatformIO/Arduino, pioarduino C6
platform), same HTTP debug + WS controller + HTTP-pull OTA ergonomics, same OTA teardown
discipline, same host-test discipline for the pure-C++ core.

**Status:** firmware + HA component implemented and **hardware-verified on a TinyC6 + live
motor (2026-06-05)**. Pure-C++ core host-unit-tested (`pio test -e native`, 29 cases). Full
design rationale: [`SPEC.md`](SPEC.md). **fw 1.14.2 is live on all 12 controllers (2026-09-05 06:20 UTC,
canary rule: Bed 2 first, 3 min clean, then eleven gated pulls; HA `somfy_sdn` 1.13.0)** — the actron
twin's bridge is still on 1.11.0 (see its CLAUDE.md). The generations it carries, oldest first:
**fw 1.11.0** is the network-stack watchdog (`netwatch.{h,cpp}`) on top of 1.10.0's clock
re-baseline + fault registry; see "Network-stack watchdog" below. **fw 1.12.0** adds
read-only WiFi MAC transmit telemetry + lwIP pcb snapshots (`txstats`/`tcpsnap`) for the
long-frame uplink-loss investigation — see "MAC transmit telemetry" below. **fw 1.13.0** is the A/B instrument for the physical cause: a runtime,
NVS-persisted **WiFi protocol knob** (`bgnax` = the default, written explicitly since 1.14.1; `bgn` = no 11ax, `bg` = no
11n) switchable per unit from HA / HTTP / WS / console without a reflash, plus `POST /phycal`
(erase the stored PHY calibration and reboot into a full RF cal). It also fixes the 1.12.x pcb
snapshot that never matched a socket (dual-stack sockaddr) and drops the raw `tx_rtt` — see
"WiFi protocol A/B" below. **fw 1.14.0** is the
robustness build for the mechanism the pcap proved: the library's pong reaper is gone in favour
of a host-tested liveness policy (`ws_liveness.{h,cpp}`), no hot-path WS frame exceeds 600 B
(per-record diag backlog, three-frame health push), and `/wifiproto` reboots to apply — see
"WS liveness" below.

**Hardware bring-up results (bench, motor `16:5A:AB`, MAC `404cca512e64`):**
- TX/RX, frame inversion + big-endian checksum (build *and* parse), and the retry/serialised
  bus access all verified on silicon at 4800 8-O-1.
- Discovery (`SET_NODE_DISCOVERY` + broadcast `GET_NODE_ADDR`) finds + registers the motor.
- Directed reads confirmed: position `0x0C→0x0D`, limits `0x21→0x31`, direction `0x22→0x32`,
  status `0x0E→0x0F`. Command/ACK path confirmed (`CTRL_STOP` → ACK `0x7F`).
- **`SET_MOTOR_DIRECTION` payload CONFIRMED** (`0x00`=std / `0x01`=reversed; writes ACK and read
  back) — resolves open item §13.2 (the forced-re-cal question is still untested).
- WiFi provisioning, HTTP debug API over WiFi, zeroconf advert, and **HTTP-pull OTA all verified
  end-to-end** (OTA teardown held; `fw` build-string bumped post-flash, no brick).
- Real-motor frame sizes differ from the spec sketch: `POST_MOTOR_POSITION` carries **11** data
  bytes, `POST_MOTOR_STATUS` **4**. `parsePosition` only consumes the first few, so it's fine.

**In-fixture results (motor `16:4D:92`, on a roller tube):** full commissioning + control all
confirmed working — `CTRL_MOVE` timed-nudge jog (the only move that works before limits),
`SET_MOTOR_LIMITS` (top at current = 0 reference, bottom at current *or* an absolute pulse count),
`SET_MOTOR_DIRECTION`, `SET_FACTORY_DEFAULT` (reset limits), and normal cover open/close/position.
On a bare bench motor (no roller) moves NACK `limits_not_set`/`wrong_position` and position reads
unknown — that's the motor refusing without a position reference, not a firmware bug.

**Serial console (USB-CDC, `main.cpp` `pumpConsole`)** — bench/field debug without WiFi (works in
portal mode too): `s` `d` `mode active|listen` `disc` `errs` `clear` `send <addr|bc> <msghex>
[data…]` `get <addr> pos|lim|dir|status` `move <addr> open|close|stop|pos`
`jog <addr> up|down [dur]` `setbottom <addr> <pulses>` `forget <addr>` `cfg <baud> <n|e|o>`
`probe [ms]`. `probe` forces a broadcast `GET_NODE_ADDR` and dumps raw RX (transceiver bring-up).

## Protocol (the proven baseline)

Framing/checksum/inversion are ported verbatim (semantics) from the field-proven
`matter-apps/common/features/app_sdn.cpp`. Wire rules (`src/sdn.h`):

- Raw frame: `[MSG_ID][LEN|flags][NET][SRC×3][DST×3][DATA..][CKSUM×2]`.
- Bus frame: every byte **except the 2-byte checksum** is bitwise-inverted (`~b`).
- Checksum: 16-bit sum of the **inverted** bytes preceding it, **big-endian**, appended
  un-inverted. (The only big-endian field; DATA multi-byte values are little-endian.)
- Addresses: 3 bytes, **byte-reversed** from display form (`01:00:23` → `{0x23,0x00,0x01}`).
  Tool source = `01:00:00`. Broadcast = `{0xFF,0xFF,0xFF}`.
- Line: 4800 baud, **8-O-1**, half-duplex.

**Position semantics (load-bearing):** Somfy `pct` 0 = up/open, 100 = down/closed. HA cover
0 = closed, 100 = open. Conversion lives in **one place** — `sdn::haToSomfy` / `sdn::somfyToHa`
(`ha = 100 − somfy`). Firmware keeps **native Somfy %** everywhere; HA inverts only at the WS/HA
boundary (the firmware applies the inverse for `set_position`). Read motor position from
`POST_MOTOR_POSITION data[2]` directly (the percent byte) — do **not** recompute from pulses
(the position-reporting fix carried from the matter-apps commit).

**Spurious-percent guard (fw 1.1.5):** `devices.cpp applyPosition` suppresses a percent change when
the motor is **idle** and the encoder `pulses` count is **unchanged** — a stationary blind with a
frozen encoder physically cannot have moved, so a changed percent byte is a glitch. This kills a
field-observed flap where the motor momentarily reports `percent=100` (HA cover slams *closed* then
*open* ~30 s later) while pulses stay put. It only suppresses on an exact pulse match, so a real
move (which always advances the encoder) is never affected. Unit-tested in `test_devices`.

## Architecture / files

Two surfaces like Actron: an HTTP API for RE/operator use, a WS API as the HA runtime surface,
a dedicated FreeRTOS task owning UART1, and HTTP-pull OTA. New vs Actron: a **device table**
(multi-motor), an **error ring buffer**, and **WiFi provisioning** (no baked creds).

| File | Purpose |
|------|---------|
| `platformio.ini` | `um_tinyc6` (Arduino, pioarduino C6 platform) + `um_tinyc6_ota` (espota) + `[env:native]` host tests (compiles `sdn.cpp`/`errlog.cpp`/`devices.cpp`/`mono.cpp`/`fault.cpp`/`netwatch.cpp`/`txstats.cpp`/`tcpsnap.cpp`/`ws_liveness.cpp` only — txstats/tcpsnap are Arduino-free outside an `#ifdef ARDUINO`). |
| `src/sdn.{h,cpp}` | **Pure C++, no Arduino deps.** Framing/checksum/inversion + command-payload builders + response parsers + address & HA-inversion helpers. Host-tested. |
| `src/errlog.{h,cpp}` | **Pure C++.** Bounded ring buffer (128) of wire/protocol events + per-class counters. Host-tested. |
| `src/devices.{h,cpp}` | **Pure C++.** Device table keyed by node addr — registration, position/limit application, stall + fault detection, comms-loss sweep. Host-tested. (This is the firmware's state model; there is intentionally no separate `state.cpp` — the table *is* the snapshot, serialised in `ws_api`/`http_api`.) |
| `src/mono.{h,cpp}` | **Pure C++ core + a thin Arduino wrapper.** Fault-tolerant monotonic clock — `mono::now()` replaces `millis()` for every deadline, stamp and age in the firmware. Rejects high-word faults outright and re-baselines rather than clamping for ever. Host-tested. See "Clock faults" below. |
| `src/fault.{h,cpp}` | **Pure C++, host-tested.** Device-level fault registry: a bitmask of named conditions in severity order, each with a one-line detail. Surfaced in `/stats` (`fault=`/`fault_detail=`), the WS health push, and HA's `sensor.<controller>_fault` + `binary_sensor.<controller>_problem`. The evaluator that decides when to raise each code lives in `main.cpp` (`updateFaults`), where the live inputs are. Added fw 1.10.0 because Bed 2's nine-hour wedge left every fault signal in the estate clear — the only fault entity was per-motor and knew nothing about the controller hosting it. |
| `src/netwatch.{h,cpp}` | **Pure C++, host-tested** (fw 1.11.0, ledger shq-suite-0044; twin of `actron-sniffer/src/netwatch.{h,cpp}`). The network-stack watchdog *policy*: three triggers (adopted backward clock step >= 60 s; gateway unreachable 3 min with nothing inbound over WS; heap < 60 kB for 10 min), each answered by a re-association first and a reboot only if that did not help. Fed at 1 Hz by the glue in `wifi_prov.cpp`, which owns the gateway ICMP probe (IDF `esp_ping`, one echo a minute) and performs the actions. See "Network-stack watchdog" below. |
| `src/bus.{h,cpp}` | Arduino. The **only** code touching UART1. FreeRTOS task: LISTEN/ACTIVE TX gate, command queue + raw request/response, retry, polling cadence, passive sniffing → device table + sniffer ring + errlog, OTA teardown. |
| `src/ws_api.{h,cpp}` | WebSockets controller API (port 8767). Push `state` snapshots, command/`ack`/`error`, heartbeat. Broadcasts only from the main loop (dirty-flag set by the bus task). **Keepalive + eviction (fw 1.1.5 → 1.14.0):** `begin()` used to call `enableHeartbeat(15000,5000,2)` so a half-open client left by a WiFi blip (no TCP FIN) could not linger until lwIP's retransmit timeout stalling the WS loop (the multi-minute `unavailable` stretches on weak-signal motors, fixed in 1.1.5). **Since fw 1.14.0 the library heartbeat is OFF** — its reaper evicted a live HA 10 s into an uplink fade (ledger shq-suite-0046) — and `GuardedWebSocketsServer` pings through the write-guard and judges liveness on `ws_liveness.h` instead; the diag backlog is a record per frame from a per-client cursor and the health push is three frames (see "WS liveness" below). **Wedge watchdog (fw 1.3.0):** the heartbeat only evicts *non-responsive* clients — it can't help when all `WEBSOCKETS_SERVER_CLIENT_MAX` (5) slots fill with *live* zombies (a client leaking duplicate connections, each kept alive by its own keepalive). At capacity the library refuses every new handshake (accept→drop, no HTTP response) and the device looks dead to HA while HTTP/the bus stay healthy. A controller only ever has one legitimate HA coordinator, so `loop()` reboots if `connectedClients()` stays at the cap continuously for `WEDGE_REBOOT_MS` (5 min); any drop below the cap resets the timer (reboots via `wifi_prov::noteReboot("ws-wedge")` as of 1.5.0, so the cause survives into the next boot's `/stats`). This is the backstop for an HA-side connection leak fixed at source by the coordinator's close-before-reconnect (`somfy_sdn` 1.4.1) — it first wedged `bed_1_blinds_left`'s controller on 2026-06-14. **Silent-socket telemetry (fw 1.5.0, ledger shq-suite-0022):** lifetime `onEvent` counters `ws_conn`/`ws_disc`/`ws_err` (ported from actron-sniffer) surfaced in `/stats` + `/stats.json` — a growing `ws_conn − ws_disc` gap means sockets are dying without the WS library seeing a DISCONNECTED (the silent-death signature of the Actron/somfy ~5-day-fuse failure). **Heartbeat-wedge fix (fw 1.5.1, ledger shq-suite-0034 — the actual root cause of the "silent socket"):** the periodic push was gated on `(int32_t)(t - g_last_heartbeat_ms) >= (int32_t)HEARTBEAT_INTERVAL_MS`, which reads as "not due yet" for as long as the stamp sits in the future — and an occasional far-future `millis()` read (see "Clock glitches") put it there. The socket, the WS-level pings and the connect-time snapshot all keep working, so HA goes available on connect, silent for 30 s, unavailable, reconnect — a 40 s flap cadence that ran 8 h on Bed 2 (701 cycles) and has hit gym, bed 4 and living-room-left before it. Now an **unsigned** elapsed test, so a future stamp wraps to a huge elapsed value and fires on the very next loop; `mono::now()` is the second layer. `hb_tx`/`hb_age` in `/stats` make it a one-curl diagnosis. |
| `src/diag.{h,cpp}` | **Self-diagnostics** (fw 1.6.0, ledger shq-suite-0038 — twin of `actron-sniffer/src/diag.{h,cpp}`, keep them in step). Classifies every WS disconnect from evidence held at the instant it fires and stamps it with the machine's condition; 48-entry RAM ring served at `/diag`, `/diag.json`, pushed over WS live and replayed as a backlog on connect. See "Self-diagnostics" below. |
| `src/ws_guard.{h,cpp}` | **Write-guard + keepalive + liveness glue** (fw 1.7.0 → 1.14.0, twin — byte-identical with actron's). `GuardedWebSocketsServer`: zero-timeout `select()` before every write, `broadcastWritableTXT()`/`sendWritableTXT()` (essential frames), `sendTelemetryTXT()`/`broadcastTelemetryTXT()` (also held off a retransmitting pcb), `sendPings()` (our 15 s keepalive, guarded), `judge()` (applies `ws_liveness::judge` per slot and evicts). Counts `skipped_writes`, `deferred_reaps`, `liveness_evicts`, `liveness_extended`, `deferred_telemetry`, `pings`, `ping_skips`, `big_frames`. |
| `src/ws_liveness.{h,cpp}` | **Pure C++, host-tested** (fw 1.14.0, twin). The client liveness policy and every keepalive constant, each `static_assert`ed against lwIP's retransmit ladder and HA's 40 s window: `LIVENESS_MS` 45 s, `LIVENESS_HARD_MS` 120 s, `STALL_REAP_MS` 30 s, `MIN_AGE_MS` 10 s, `FRAME_BUDGET_BYTES` 600. See "WS liveness" below. |
| `src/txstats.{h,cpp}` | **WiFi MAC transmit telemetry** (fw 1.12.0, twin of `actron-sniffer/src/txstats.{h,cpp}`). Enables the ESP32-C6 driver's per-AC transmit statistics at runtime and reads+clears them once a second from the 1 Hz net tick in `wifi_prov.cpp`: acknowledged frames, EDCA/TB retries, ACK timeouts, collisions, buffer starvation, the failure-state matrix, plus the negotiated PHY mode / channel / AP PHY set. Pure accumulator (`Counters`/`Accumulator`) is host-tested. **Read-only** — the baseline half of the long-frame uplink-loss A/B. See "MAC transmit telemetry" below. |
| `src/tcpsnap.{h,cpp}` | **lwIP pcb snapshot** (fw 1.12.0, twin). Walks `tcp_active_pcbs` under `LOCK_TCPIP_CORE()` to read one connection's `state`/`nrtx`/`rto`/`cwnd`/`snd_buf`/`snd_wnd`/unacked bytes by the peer address behind a socket fd. Sampled at 1 Hz per WS client from `ws_api::loop()`, captured live at a stall-reap before the socket is closed, and carried on `ws_disconnect`/`ws_stall_reap` records + the health push. **1.13.0 fixes a peer-match bug that made every 1.12.x capture return false** (the core's `NetworkServer` is an AF_INET6 dual-stack listener, so `getpeername()` on an accepted socket reports an IPv4-MAPPED IPv6 sockaddr, never `AF_INET`); `tcp_miss` in the health push now counts captures that found no pcb, so the instrument reports its own failure. |
| `src/wifi_proto.h` | **WiFi protocol A/B knob** (fw 1.13.0, header-only, twin of `actron-sniffer/src/wifi_proto.h`, host-tested). `Proto{BGNAX,BGN,BG}`, the slug (`name()`/`parse()`) used by NVS, `/stats proto=`, the health push, the WS command and HA's select, and the `esp_wifi_set_protocol()` bitmap (`0` for `bgnax` = make no call). Glue in `wifi_prov.cpp`: `readProto()` at boot, `applyProto()` after `WiFi.mode()` and before every `WiFi.begin()`, `setWifiProto()`, `erasePhyCalibration()`. See "WiFi protocol A/B" below.  ⚠️ 1.14.1: `bgnax` is an explicit bitmap, not \"no call\" — the IDF driver persists the last `esp_wifi_set_protocol` value in its own NVS, so a unit once switched to `bgn` stayed HT20 through every later \"bgnax\" reboot (ledger shq-suite-0046). |
| `src/http_api.{h,cpp}` | HTTP debug API (port 80). `/stats /stats.json /devices /log /errors` (GET) and `/mode /send /discover /move /forget /wifi /reconnect /update /clear /reboot` (POST). `/send` is the RE workhorse. `/update` carries the **OTA app-guard** (see Device identity below). |
| `src/app_desc.cpp` | **Native `esp_app_desc` override (Option A).** A strong `extern "C"` `esp_app_desc` in section `.rodata_desc` shadows the prebuilt Arduino one (`project_name="arduino-lib-builder"`), so the image's native descriptor reports `project_name="somfy-sdn"` + the real `SOMFY_FW_VERSION`. Read by `esp_ota_get_partition_description()` in the OTA guard and by `esptool image_info`. See "Device identity". |
| `src/version.h` | `SOMFY_FW_VERSION` semver — bump on every flash (see root CLAUDE.md → Versioning). |
| `src/wifi_prov.{h,cpp}` | NVS creds (Arduino `Preferences`), STA connect w/ retries, SoftAP captive portal, GPIO0 button (long = wipe, short = wink all), mDNS, configured-motor loading. **WiFi hardening (fw 1.5.0, ledger shq-suite-0022 — three in-wall controllers wedged off-network after infra outages, needing a breaker power-cycle):** after the one-shot boot connect the firmware previously relied entirely on the Arduino stack's implicit auto-reconnect; when that got stuck (the known ESP32 glitch class after an AP reboot/rekey/channel change) the device was stranded forever. Now layered: (1) **active link-retry** — after 20 s of continuous downtime (auto-reconnect gets the easy cases first), force a full `WiFi.disconnect()` + `begin()` every 30 s, resetting a stuck association state machine and re-applying the all-channel strongest-AP scan; (2) **WiFi-death reboot watchdog** — STA link down continuously for 5 min ⇒ `noteReboot("wifi-dead")` (backstop if even re-begin can't recover); (3) **mDNS re-announce on every (re)association** (`GOT_IP` event → `MDNS.end()` + restart from `loop()`): a reconnect may land on a NEW IP and HA's zeroconf host-healing only works if the advert re-fires — ESPmDNS's own IP-change behaviour is not dependable; (4) **event telemetry** — `WiFi.onEvent` counts lifetime STA disconnects + last 802.11 reason code, surfaced as `wifi_disc=`/`wifi_reason=` in `/stats` (handler stays minimal; logging happens from `loop()`). **Portal-purgatory retry (fw 1.5.0):** a device that boots while the AP is down (e.g. post-power-outage, AP slower to start) used to fall into the portal and stay there forever despite valid creds; with creds present the portal now reboots to retry STA every 15 min (`noteReboot("portal-retry")`); a creds-less portal never retries. **`noteReboot(reason)`/`bootNote()`:** records the reason for a deliberate self-reboot in NVS; the next boot reads + clears it and surfaces it as `note=` in `/stats` / `boot_note` in `/stats.json` (alongside `reset=` from `esp_reset_reason()`, so power-cycle vs watchdog vs crash is distinguishable per fleet sweep). **STA connect uses `WIFI_ALL_CHANNEL_SCAN` + `WIFI_CONNECT_AP_BY_SIGNAL`** (not the Arduino-default `WIFI_FAST_SCAN`, which joins the *first* matching BSSID — often a cached distant AP — and sticks there with no roaming), so every (re)connect joins the **strongest** AP for the SSID; a reboot/OTA now lands on the nearest AP. The stack has no live roaming once associated, so `requestReconnectBestAp()` (HTTP `POST /reconnect` / WS `reconnect_wifi` / HA button) forces an on-demand re-scan + reassociate — deferred to `loop()` so the ack flushes before the link drops. Hostname/SoftAP SSID = `somfy-sdn-<XXXX>` where `XXXX` is the last 2 octets of the **STA MAC** (`esp_read_mac(ESP_MAC_WIFI_STA)`), so it matches the device's label/MAC. (Do **not** use `ESP.getEfuseMac() & 0xFFFF` — that's the shared vendor OUI; every TinyC6 came out `4C40`. getEfuseMac also returns the *base* MAC, which differs from the STA MAC on the C6.) |
| `src/main.cpp` | Boot wiring: bus task → WiFi/provisioning → HTTP+WS (when connected). |
| `test/test_sdn`, `test/test_devices`, `test/test_mono`, `test/test_fault`, `test/test_netwatch`, `test/test_txstats`, `test/test_wifi_proto`, `test/test_ws_liveness` | Unity host tests (78 cases). `pio test -e native`. |
| `WIRING.md` | Parallel-tap wiring (single transceiver, no terminator on a mid-bus tap). |

### Clock faults — and why the filter, not the hardware, caused the worst outage

`millis()` on these C6 boards misbehaves in both directions, and the firmware has been bitten by
each. **The 2026-08-31 Bed 2 outage was caused by our own defence against the first fault**, so
read both halves of this before touching `mono.{h,cpp}`.

**Fault A — far-future reads (fw 1.5.1, ledger shq-suite-0034).** `GET /errors` on the Bed 2
controller once held six entries stamped up to **780,083 s on a device 31,300 s into its boot**.
That ring is RAM-only and zeroed at boot, so those stamps came from live `millis()` calls. Any
deadline variable that captures one is poisoned until the real clock catches up — hours to days.

**Fault B — a backward step that stays (fw 1.10.0, ledger shq-suite-0041).** On 2026-08-31 Bed 2's
`millis()` dropped by **exactly 6 × 2³² microseconds (25,769 s)** and kept running from there: the
low 32 bits of the 64-bit microsecond counter were preserved and only the high word changed. The
old filter's monotonic clamp — `if (delta < 0) return last_;`, with no escape hatch — then pinned
`mono::now()` at a constant for **precisely as long as the step was large**. Nine hours during
which no deadline in the firmware fired at all: no state push, no heartbeat, no RS485 poll. HA saw
a device that accepted connections and sent nothing, flapping `unavailable` every 40 s. It
self-recovered the moment the hardware clock climbed back to the pinned value.

The lesson is the shape, not the number: **a clock defence that can wait indefinitely converts the
size of a glitch into the length of an outage.**

Three layers of defence now, in the order they act:

1. **High-word detection.** `esp_timer_get_time()` returns 64-bit microseconds and `millis()` is
   that ÷ 1000, so a corrupt high word moves `millis()` by a whole multiple of 2³² µs
   (`WORD_STEP_MS`, 4,294,967.296 ms). Real elapsed time is never a near-exact multiple of that —
   `now()` is called continuously, so a genuine 71.6-minute gap between reads cannot happen. Such
   a step is provably corrupt in **either** direction and is rejected on the first read.
2. **Bounded clamp.** Rejection stops after `REBASE_AFTER_REJECTS` (1000) consecutive samples. Past
   that the clock has moved and stayed moved, so `mono::now()` adopts it and counts a `rebase`.
   Time going backwards once costs a single early deadline firing; refusing costs the whole device.
3. **Unsigned elapsed-time comparisons** at the call sites. `(uint32_t)(now - last) >= interval`
   wraps a future stamp to a huge elapsed value and fires *immediately*; the old `(int32_t)(...)`
   form waits for the real clock to catch up. Prefer the unsigned elapsed form over an absolute
   `deadline` variable wherever the choice exists. **Note this protects a poisoned deadline
   VARIABLE, not a poisoned CLOCK** — that gap is what layer 2 exists to close.

**The double-read is gone (fw 1.10.0).** `mono::now()` used to sample twice and take the earlier of
the pair. It was retired having never once fired: `clk_torn` read **zero on all twelve controllers
and the actron bridge**, across four days of uptime each. It also could not have helped with Fault
B — both reads returned the same wrong value — and for a downward glitch "take the earlier" would
have deliberately selected the *bad* sample. One read per call now, which matters in the bus task's
tight deadline-poll loops.

**Counters** surface as `clk_back` / `clk_word` / `clk_rebase` / `clk_rebase_ms` / `clk_jump` /
`clk_jumpms` in `/stats`, and as HA sensors. **Read `clk_back` as a RATE**: a healthy controller
gathers a few hundred over four days (~0.002/s); thousands per second means the clock is pinned
*right now*. See the correction note under Self-diagnostics.

**Diagnosing a recurrence** — any controller whose HA entities flap `unavailable`/`available` on a
~40 s cadence:

```bash
curl -s http://<ip>/stats     # hb_age >> 10000 with hb_tx frozen = the heartbeat has stalled;
                              # wifi_disc=0 and ws_conn≈ws_disc climbing ~90/h confirm it is NOT WiFi
```

A stalled heartbeat can be unwedged **without a reboot** by sending any WS command (e.g.
`{"type":"command","command":"set_mode","mode":"active"}`) — that sets the dirty flag, which
re-stamps the heartbeat. Useful when you want the blind working right now and the diagnosis later.

Note the knock-on: while flapping, each HA reconnect costs ~170 B of heap that a wedged controller
never gives back (healthy ones release it within ~2 min). 700 flap cycles took Bed 2 from 240 kB to
122 kB — a halved heap is a *consequence* of the flap, not its cause. Don't chase it as a leak.

### Network-stack watchdog (`src/netwatch.{h,cpp}`, fw 1.11.0) — the filter cannot heal the layer beneath it

The 1.10.0 re-baseline worked exactly as designed on 2026-09-02, and the device still died
(ledger shq-suite-0044). `millis()` is `esp_timer_get_time()/1000`, and every esp_timer alarm in
the IDF — the WiFi driver's included — is an **absolute** target on the same hardware counter.
When Bed 2's counter stepped back by 35 x 2^32 us, `mono` kept every deadline in *this* firmware
firing, but the driver beneath it was timer-dead for 41.75 h and leaked ~3.3 B/s until, 18 h
later, the receive path starved. On the wire the station stayed associated and transmitted a
gratuitous ARP every 60 s while answering nothing: no ARP reply, no ICMP, no TCP, no mDNS. The
router said "healthy client", HA said "unreachable", and both were right. `WIFI_DEAD_REBOOT_MS`
and the WS wedge watchdog never armed because both key on a *down* link. A router-side client
reconnect cured it in one second without a reboot (heap 8 k -> 240 k) **and the leak did not
resume**: a re-association tears down and re-arms the driver's timers against the current
counter. The Gym controller confirmed the mechanism the same day (31.6 min step, same leak rate,
un-leaked the instant its counter climbed back).

So the watchdog's first response is always the thing that worked — drop and rejoin the
association (`reassociate()`, the same path as the manual `POST /reconnect`), which keeps the
RAM diagnostic ring — and a reboot is the second tier only:

| Trigger | Re-associate | Reboot (`note=`) |
|---------|--------------|------------------|
| Adopted **backward** clock step >= `CLOCK_STEP_REASSOC_MS` (60 s) | immediately | never — the step is already handled; this just refreshes the driver |
| Gateway unanswered for `UNREACH_REASSOC_MS` (3 min) with the link up **and nothing inbound over WS for 60 s** | then | `stack-dead` if still unreachable `UNREACH_REBOOT_MS` (5 min) after |
| Heap < `HEAP_LOW_BYTES` (60 kB) for `HEAP_REASSOC_MS` (10 min) | then | `heap-low` if still low `HEAP_REBOOT_MS` (10 min) after |

Re-associations are spaced by `REASSOC_COOLDOWN_MS` (5 min) whatever the trigger; reboots ignore
the cooldown. **Inbound WS traffic (any frame or pong, `wifi_prov::noteInbound()` from
`ws_api`'s `onEvent`) vetoes the unreachable trigger** — that is what makes it mean "nobody can
talk to me" rather than "the gateway dropped ICMP", and why an HA outage on its own can never
trip it (host-tested: `test_ha_outage_alone_never_acts`). The probe is one ICMP echo to
`WiFi.gatewayIP()` a minute via the IDF's `esp_ping` (short-lived task, frees itself in
`on_ping_end`); a session that cannot even be created counts as unanswered, which under heap
starvation is the right verdict. The policy is pure and Arduino-free (`netwatch::Policy::step`),
so every rule above has a host test in `test/test_netwatch`.

Telemetry: `nw_fail` (consecutive unanswered probes — climbing with the link up IS the
signature), `nw_probes`, `nw_recover` (re-associations performed), `nw_reason` in `/stats`,
`netwatch{}` in `/stats.json`, `nw_fail`/`nw_recover`/`nw_reason` in the health push (HA:
`sensor.<controller>_gateway_probe_failures`, `sensor.<controller>_network_recoveries`, component
1.10.0), and a `net_recover` diag event stamped with the heap/RSSI that triggered it. Why the
counter steps at all is still unknown — non-word steps (-1,111 s, -1,895 s) look like the
counter being *loaded* with a stale value, which only sleep/PM code does in the IDF, yet
`CONFIG_PM_ENABLE` is unset and `WiFi.setSleep(false)` is in `tryConnect`.

### Concurrency / OTA (carried from the Actron brick fix)

- The bus task spends its time in **blocking, CPU-yielding waits** (`vTaskDelay(1)` while waiting
  on the UART; `ulTaskNotifyTake` between transactions). SDN is request/response, so there is
  **no busy-pump** — do not introduce one (that is exactly what starved Actron's OTA download).
- `bus::otaSuspend()` runs **before** `httpUpdate()` in `/update`: force LISTEN, suspend the
  task, drain the RX FIFO. Restored on failure; success reboots.
- Build-string cache gotcha (same as Actron): change file **content** (not just `touch`) to
  refresh the `fw=` string in `/stats` after a reflash. (Note `__DATE__`/`__TIME__` live in
  `http_api.cpp` for `/stats` and `main.cpp` for the console — only the TU you edit refreshes.)

### Bus mode & discovery (as-built)

- **Mode (`LISTEN`/`ACTIVE`) is the TX gate.** LISTEN = sniff only, never transmits; ACTIVE =
  poll + accept commands + discover. It changes **only on explicit instruction** —
  `POST /mode?set=`, WS `set_mode`, serial `mode`, or briefly during the portal's motor-scan —
  and is forced to LISTEN around OTA (restored after). Motor commands are rejected in LISTEN.
- **Boot default is ACTIVE** (`g_mode` in `bus.cpp`), a deliberate deviation from SPEC §6.2.
  Rationale: this is the sole controller on a dedicated bus, so it should work after a reboot
  with no manual arming. **If a competing controller (e.g. a Somfy keypad) is ever added to the
  bus, change the default back to LISTEN** and arm ACTIVE per deployment, or the two masters will
  collide.
- **Auto-discovery:** when ACTIVE, the bus task runs one discovery sweep on boot, then retries
  every `DISCOVERY_RETRY_MS` (30 s) **only while the table is empty**; once any motor is found it
  stops (polling + passive observation keep it fresh). SDN motors don't self-announce (polled
  slaves), so enrolling a motor **added later** needs an explicit `rediscover` (HA service / HTTP
  `/discover` / serial `disc`). Active discovery is best-effort on a multi-motor bus (simultaneous
  replies collide) — SPEC §13.5.
- A just-discovered device is polled **immediately** (the poll timer treats "never polled" as due)
  so HA gets position right after boot.

## Build / test / flash

```bash
cd somfy-sdn
pio test -e native          # host unit tests (sdn + devices)
pio run -e um_tinyc6        # build firmware
pio run -e um_tinyc6 -t upload   # flash over USB-C (first build pulls the toolchain)
```

(Use `~/.pio-venv/bin/pio` on the dev box.) Wireless reflash uses **HTTP-pull OTA** (WSL can't be
an espota target): host `firmware.bin` on atlas and `POST /update?url=...`. The atlas OTA file
server is **ephemeral** — start it before every reflash (see memory `actron-ota-server-ephemeral`).
From a non-WSL LAN host you can instead `pio run -e um_tinyc6_ota -t upload`.

**Versioning:** bump `SOMFY_FW_VERSION` in `src/version.h` on every change you flash (and the HA
component's `manifest.json` version on HA changes) — see root `CLAUDE.md` → "Versioning". `fw=` in
`/stats` shows `<semver> (<build date>)`; bumping the version also refreshes the build date (so it
doubles as your OTA-took confirmation).

**OTA client timeout is 30 s (fw 1.14.2).** The library default of 8 s is shorter than lwIP's initial
retransmit ladder (3/6/12 s), so on a marginal uplink the device's own 525 B `GET` could be lost twice and
the pull abandoned before the third copy landed — Living Left failed two pulls exactly that way while its
329 B state pushes kept flowing (ledger shq-suite-0046). A failed pull writes nothing; retry in a quiet window.

**OTA server reliability (learned the hard way):** detaching the atlas `http.server` over SSH
(`nohup`/`setsid`/`disown`) raced and frequently left it dead when the device pulled. The robust
way is to hold it open with a single live background SSH for the duration of the pull:
`ssh atlas 'cd ~/somfy-ota && exec python3 -m http.server 8088'` (run in background), verify it
serves, `POST /update`, confirm the new `fw=`, then kill the SSH.

### Fleet OTA run-book (flashing ALL Somfy controllers) — derive targets from HA, NEVER a scan

**Cardinal rule:** the set of "Somfy devices" is answered **only by the `somfy_sdn` HA
integration**. NEVER enumerate flash targets with a network/ARP scan or a MAC-OUI filter — every
ESP32-C6 board here (Somfy controllers, the **actron-sniffer** which is the same board and shares
the *identical* `POST /update` OTA endpoint, remote-mouse, …) shares the `40:4c:ca:51` Espressif
OUI, so a scan sweeps in non-Somfy devices and can push the wrong firmware onto them. A 2026-06-25
flash run did exactly this — an OUI scan put the Actron controller on the target list; it was only
caught at the per-device fingerprint step. Membership = HA; the network is consulted *only* to find
where an already-confirmed-Somfy device currently lives.

1. **Build + test.** `pio test -e native` (must pass) → `pio run -e um_tinyc6` → bump
   `SOMFY_FW_VERSION`. Stage `firmware.bin` on atlas; start the OTA server (above).
2. **Derive the target set from HA (the ONLY membership source).** List config entries with
   `domain == "somfy_sdn"`; for each, find its **controller device** in the device registry
   (identifier `("somfy_sdn", "<mac>")`, `via_device_id == None`) and read its `mac` +
   `configuration_url` (`http://<host>/` → the self-healed IP). This yields `{title, mac, ip}` per
   controller. Do **not** add any IP that didn't come from this list.
3. **Fingerprint-gate every IP before flashing** (belt-and-braces against a stale/wrong IP):
   `GET /stats.json` and require **`app == "somfy-sdn"`** (fw ≥ 1.4.0; older fw: fall back to the
   `/stats` shape — Somfy has `devices= online=`, Actron has `A.bytes= bridge= baud=`) **and** the
   reported `mac` to match the registry MAC for that entry. Any non-Somfy `app`/fingerprint or MAC
   mismatch → **ABORT that target**, don't flash it. (As of fw 1.4.0 the device *also* self-guards
   — `/update` refuses a foreign image — but that's the last line of defence, not a substitute for
   gating here.)
4. **Present the manifest + confirm.** Show `{title, mac, ip, current_fw → target_fw}` and check
   the count equals HA's `somfy_sdn` entry count. Flash only on explicit go-ahead.
5. **Canary then batch.** The standing canary is the **"Jordon Study" controller** (MAC suffix
   `3D24`; on a reserved IP — value in the private config). Always flash it first: it's the easiest
   to physically pull off the wall for USB recovery if an OTA goes wrong.
   Confirm it returns on the new `fw=`, the motor is healthy, and the HA coordinator reconnects
   after the reboot — then flash the rest.
6. **Verify all + retry stragglers** (one OTA retry is normal). Confirm every target reports the
   new version. **Tear down the OTA server.** Update docs/ledger.

**Abort conditions:** any IP whose `/stats` isn't Somfy; any device→registry MAC mismatch; the
resolved-IP count ≠ the HA `somfy_sdn` entry count; or a device already on an unexpected version.

### Device identity & OTA app-guard (fw 1.4.0)

Three layers make a wrong-firmware flash hard, because this board, the actron-sniffer, and every
other TinyC6 here share the `40:4c:ca:51` OUI **and** the `POST /update` endpoint:

- **Native app id (`app_desc.cpp`, Option A).** The image's `esp_app_desc.project_name` is
  `"somfy-sdn"` and `version` is the real `SOMFY_FW_VERSION` — readable from the running app, the
  flash, or `esptool image_info`. (Actron's is `"actron-mitm"`.) The override works because a
  strong `extern "C" esp_app_desc` in `.rodata_desc` stops the linker pulling the prebuilt
  `arduino-lib-builder` copy from `libesp_app_format.a`. **Verify after a build:** the magic word
  `0xABCD5432` in `firmware.bin`, then `project_name` at +48, `version` at +16.
- **Surfaced id.** `/stats` starts `# app=somfy-sdn …`; `/stats.json` carries `app` + `model`.
  This is what the run-book's fingerprint step checks.
- **OTA app-guard (`handleUpdate`).** `/update` runs `httpUpdate` with `rebootOnUpdate(false)`,
  then reads the just-written boot partition's `esp_app_desc`; if `project_name != "somfy-sdn"` it
  **reverts the boot partition to the running app and does not reboot** — a foreign image is
  written but never executed. Validated live on the Jordon Study canary (2026-06-26): a somfy image
  flashed normally; the actron image was downloaded then **rejected** (device stayed on 1.4.0, no
  reboot — uptime continuous). Note the early HTTP 200 ("pulling firmware") is sent before the
  verdict, so a rejection shows only in the serial log + an unchanged `/stats` `fw=` — poll `/stats`
  to confirm a flash actually took.

## HTTP API (port 80, LAN, all open)

| Endpoint | Purpose |
|----------|---------|
| `GET /` | human-friendly **HTML dashboard** (auto-refreshes every 3 s; polls `/stats.json` + `/devices` client-side) |
| `GET /help` | the old text endpoint listing + status line (RE/curl workflow) |
| `GET /stats` | status line, text (mode, devices, counters, fw, rssi, ip; **1.5.0 adds** `heap/minheap/maxblk/uptime`, `ws/ws_conn/ws_disc/ws_err`, `wifi_disc/wifi_reason`, `reset=`/`note=` — the silent-socket + link-churn telemetry; note `ws_clients=` was renamed `ws=` for actron parity; **1.5.1 adds** `hb_age`/`hb_tx` and `clk_torn`/`clk_back`/`clk_jump`/`clk_jumpms`; **1.6.0 adds** `sock`/`loop_max`/`http_max`/`stalls`/`pongto`/`peerclose`/`txerr`/`diag_seq` — see Self-diagnostics; **1.10.0 replaces** `clk_torn` with `clk_word`/`clk_rebase`/`clk_rebase_ms` and adds `fault=`/`fault_detail=`, and read `clk_back` as a RATE not a total — shq-suite-0041 corrects shq-suite-0039's claim that it is noise; **1.11.0 adds** `nw_fail`/`nw_probes`/`nw_recover`/`nw_reason` — see Network-stack watchdog; **1.12.0 adds** `tx_ok`/`tx_retry`/`tx_tbretry`/`tx_to`/`tx_coll`/`tx_nomem`/`tx_fail`/`tx_en`/`phy`/`ch` — see MAC transmit telemetry; **1.13.0 adds** `proto=` — the configured protocol set (`bgnax`/`bgn`/`bg`), to be read against `phy=`, which is what was actually negotiated — see WiFi protocol A/B) — consumed by the OTA flash scripts |
| `GET /diag` | **self-diagnostics** (1.6.0) — health line + the WS-disconnect event ring, newest last. Reachable when the WS layer is precisely what has stopped working. Read it ONCE; polling it is what perturbs the fault |
| `GET /diag.json` | same, machine-readable (`{"health":{…},"events":[…]}`) |
| `GET /stats.json` | controller status as JSON (fw, build, mode, hostname, ip, mac, ssid, rssi, uptime, heap, counters; **1.5.0 adds** `heap_min`, `heap_maxblk`, `ws_conn`/`ws_disc`/`ws_err`, `wifi_disc`/`wifi_reason`, `reset_reason`, `boot_note`; **1.5.1 adds** `hb_age_ms`, `hb_tx`, `clk{torn,back,jumps,last_jump_ms}`; **1.12.0 adds** `wifi_tx{ok,enable,complete,retry_edca,retry_tb,tb_times,rx_ack,rx_ba,timeout,collision,no_mem,error_a0,fail,fail_timeout,seq_max_rtt_us,enabled_acis,samples}` + `phy` + `channel`; **1.13.0 adds** `proto` and drops the raw `wifi_tx.seq_max_rtt_us`) — drives the dashboard |
| `GET /devices` | JSON device table |
| `GET /log?since=&n=` | sniffed frames (incremental, like Actron) |
| `GET /errors?n=` | error ring (newest first) |
| `POST /mode?set=listen\|active` | **TX gate** (default LISTEN) |
| `POST /send?addr=&msg=&data=` | build+send a raw SDN frame, report reply — the RE workhorse |
| `POST /discover` | discovery sweep (ACTIVE) |
| `POST /move?addr=&cmd=open\|close\|stop\|pos\|jogup\|jogdown&value=` | convenience control (jog = timed CTRL_MOVE) |
| `POST /forget?addr=` | remove a motor from the device table |
| `POST /wifi?ssid=&password=` | set creds, reboot |
| `POST /reconnect` | re-scan all channels + reassociate to the strongest AP (no reboot) |
| `GET /wifiproto` | **(1.13.0)** the configured WiFi protocol set, one slug on its own line: `bgnax` \| `bgn` \| `bg` |
| `POST /wifiproto?set=bgnax\|bgn\|bg` | **(1.13.0) the A/B knob.** Persists to NVS, replies with the new slug, then **(1.14.0) `noteReboot("wifiproto")`** — `esp_wifi_set_protocol()` is honoured only on a fresh WiFi init; the 1.13.0 live re-associate left the canary negotiating HE20 with `bgn` persisted, a reboot came up HT20. `bgnax` is the untouched default (no driver call at all). Survives reboots and OTA. See WiFi protocol A/B |
| `POST /phycal?erase=1` | **(1.13.0, HTTP only — deliberately no HA button)** `esp_phy_erase_cal_data_in_nvs()` then `noteReboot("phycal")`: the next boot performs a FULL RF calibration instead of the partial one `CONFIG_ESP_PHY_RF_CAL_PARTIAL` trims against the stored data. Last-resort remedy for a stale/bad calibration on one board; replies before rebooting |
| `POST /update?url=` | HTTP-pull OTA |
| `POST /clear` | reset ring buffers + counters |
| `POST /reboot` | deliberate restart (`?reason=` recorded into the next boot's `note=`). **Out-of-band by design:** the WS command is the one HA drives, but WS is exactly what dies in the failure modes worth rebooting for — during Bed 2's nine-hour clock wedge WS was dead throughout while HTTP answered instantly (ledger shq-suite-0041) |

Frame log line: `<seq> <t_s> +<gap>us <len>: HEX… |ascii|` (a `?` prefixes the length for
frames that failed checksum/parse).

## WS controller API (port 8767) — consumed by the `somfy_sdn` HA component

Connect `ws://<host>:8767/`. Snapshot on connect, on change, + ~10 s heartbeat. The `state.data`
object carries controller-level fields (`mode`, `fw`, `mac`, `hostname`, `ip`, `rssi`) — the HA
component names the controller device off `mac` (stable across DHCP, unlike the IP) — plus a
`devices` array. Per-device state:
`position` (**native Somfy %**, HA inverts), `pulses` (absolute encoder count — provisioning aid,
surfaced as a cover attribute), `moving`, `fault`, `online`, `up_limit`/`down_limit`,
`direction` (`normal`/`reversed`). Direction is static — the bus task queries it once per device
(alongside limits) so it reports rather than sitting unknown, **and re-reads it after a
`set_direction` (fw 1.3.1)** — without that re-read the command ACKs on the wire but the cached
`direction` never moves, so the HA "Reversed" switch snaps back to its old value and the change
looks rejected even though the motor obeyed it (ledger shq-suite-0011). Commands (envelope
`{type:"command", command, addr, id}`):
`open` `close` `stop` `set_position{position=HA%}` `jog{direction,duration}` (timed CTRL_MOVE,
commissioning) `move_steps{direction,pulses}` (post-cal) `set_top_limit` `set_bottom_limit`
`set_bottom_limit_pulses{pulses}` (absolute, specified_position) `set_direction{reversed}`
`reset` (limits) `identify` `forget` (all carry `addr`); admin:
`set_mode{mode}` `rediscover` `reconnect_wifi` (re-scan + reassociate to the strongest AP; bounces
the link, so this WS connection drops and the coordinator reconnects) `set_wifi_proto{proto}`
(fw 1.13.0: `bgnax`/`bgn`/`bg`; persists, acks, then **reboots** (fw 1.14.0 — the bitmap is only
honoured on a fresh WiFi init); HA's "WiFi protocol" select drives it; the health push's `proto`
reports the current value). Replies: `{type:"ack",id}` / `{type:"error",id,message}`.
`ack` = command accepted into the queue — there is **no optimistic state**; motor-confirmed
state arrives via the next snapshot.

**Server→client frames:** `state` (snapshot, ~330 B), `ack`/`error`, `diag` (one record each —
the connect-time backlog is delivered this way too since fw 1.14.0, from a per-client cursor),
`health` / `health_ws` / `health_net` (fw 1.14.0: the 30 s vitals as three frames a second
apart, merged by HA; ≤ 1.13.0 sent one `health`), and `diag_backlog` (≤ 1.13.0 only). Our
keepalive ping every 15 s; a client silent in both directions for 45 s is evicted (see "WS
liveness"). **No frame over 600 B on the hot path** — `ws_api.h` carries the rule.

## Home Assistant component

`home-assistant/custom_components/somfy_sdn/` — config-flow (manual host+port default 8767, **or
zeroconf auto-discovery**), push WS coordinator (ported from `actron_mitm_controller`).
`current_cover_position = 100 − somfy_pct`; per-motor `available` follows the device's `online`.
Deploy via `./setup ha` (+ `--restart`).

**Entities** (platforms: `cover`, `button`, `switch`, `number`, `sensor`, `binary_sensor`; shared
base + dynamic per-motor add in `entity.py`). Two device kinds:
- **Controller device** (one per ESP32): `button`s Rediscover motors + Reconnect WiFi (re-scan +
  reassociate to the strongest AP), `switch` Bus active (ACTIVE/LISTEN), diagnostic `sensor`s
  motors-online + wire-errors.
- **Per-motor device** (one per discovered motor): `cover` (shade) + calibration entities, all
  `EntityCategory.CONFIG`/`DIAGNOSTIC` so they sit on the device page, not dashboards — `switch`
  Reversed (stateful), `binary_sensor` Fault, `number` Jog duration (RestoreNumber, stored in
  `coordinator.jog_steps`) + `number` Bottom limit (stateful pulses: reads `down_limit`, writes
  specified_position), `button`s Set top/bottom limit, Identify, Reset positions, Jog up/down.
  Jog buttons fire the timed CTRL_MOVE nudge (works before limits); `move_steps` (pulses) stays a
  service for post-calibration.
- New motors create their HA device automatically when they appear in the state payload (i.e.
  after a `rediscover`). Calibration is *also* exposed as `services.yaml` entity-services for
  automations. Reset is a button but config-category (not dashboard-exposed) — destructive.

**Zeroconf (self-healing address):** the firmware advertises `_somfy-sdn._tcp` with TXT
`id=<MAC>` (`wifi_prov.cpp` `startMdns`). `manifest.json` declares the service type and
`config_flow.async_step_zeroconf` keys the entry on that MAC, so on every re-announcement
(reboot / new DHCP lease) it rewrites the stored host to the current IP via
`_abort_if_unique_id_configured(updates={CONF_HOST: ...})` — no manual IP, no router reservation
needed. Verified live on the LAN (advert seen from atlas: `id=404cca512e64`).

## Movement commands & NACK codes (hardware-confirmed)

Three distinct move messages (payloads cross-checked against `ccutrer/somfy_sdn` source):
- **`CTRL_MOVE` (0x01)** — momentary/deadman jog, `{direction, duration, speed}`; direction
  down=`0x00`/up=`0x01`/cancel=`0x02`, duration `0`=continuous (else `0x0A..0xFF` = timed nudge
  that auto-stops), speed normal=`0x00`/slow=`0x02`. **The only move that works BEFORE limits are
  set** (commissioning). Exposed as the HA Jog buttons (timed nudge, Set Pro style) + WS `jog` /
  HTTP `/move?cmd=jogup|jogdown&value=<dur>` / serial `jog`.
- **`CTRL_MOVEOF` (0x04)** — relative move-by-pulses/ms. Payload is correct but the motor **NACKs
  it until limits exist** (no position reference). HA `move_steps` service; post-calibration only.
- **`CTRL_MOVETO` (0x03)** — absolute (limit/IP/%/pulses); also needs limits.

**NACK reason codes** (motor→tool `0x6F`, `data[0]`; from the SDN `Nack` table): `0x01`
data_error, `0x10` unknown_message, `0x20` node_locked, **`0x21` wrong_position**, **`0x22`
limits_not_set**, `0x23` ip_not_set, `0x24` out_of_range, **`0x2F` (see below)**, `0xFF` busy.
Observed on this uncalibrated motor: `0x21`/`0x22`/`0x11` when attempting MoveOf or set-limit —
i.e. "no limits / no position reference yet", cleared once commissioned via CTRL_MOVE + limit-set.

**Reversing direction needs limits CLEARED first (NACK `0x2F`).** `SET_MOTOR_DIRECTION` only
ACKs on a motor with **no limits set**; on a commissioned motor (limits programmed) the motor
NACKs it with reason **`0x2F`** (not in the public table; established empirically on motor
`16:4D:F4`, 2026-06-25 — ledger shq-suite-0011). This mirrors Somfy's own split of "reverse
before limits" vs "reverse after limits" into two different procedures. So to flip a calibrated
blind: **Reset positions** (clears limits) → toggle **Reversed** → re-**Set top/bottom limit**.
The `0x2F` NACK is mapped to the human reason `"rejected: clear limits to change direction"`
(`bus.cpp`), surfaced in the cover `status` attribute.

**Direction-cache re-read (fw 1.3.1):** `direction` is read once at discovery and was never
refreshed after a `set_direction`; combined with no optimistic state, a successful flip (limits
clear) left the cache — and the HA "Reversed" switch — showing the old value, so it snapped back
and looked rejected even when it worked. `execCommand` now re-reads direction after
`SET_DIRECTION` (mirrors the post-limit-set re-read). This is independent of the `0x2F` block
above: 1.3.1 fixes *reporting*; the motor still won't accept the change until limits are cleared.

## Self-diagnostics (`src/diag.{h,cpp}`, fw 1.6.0, 2026-08-23)

**Twin of `actron-sniffer/src/diag.{h,cpp}` — keep the two in step**, same standing rule as
`mono.{h,cpp}`. Ported here because this fleet has the same fault and could not explain it:
**Bed 4 was caught in a locked 40 s `unavailable` flap with `hb_age=9338`** — the
heartbeat firing exactly on schedule while HA received nothing, `wifi_disc=0`, `reset=sw`, heap
240 k. Same silent-socket-at-TCP pattern as the actron bridge (ledger shq-suite-0038).

**⚠️ The 40 s cadence is NOT a device signature.** It is HA's own arithmetic —
`AVAILABILITY_TIMEOUT_S` (30 s) plus the coordinator's 10 s availability-monitor tick. A locked
40 s rhythm means "the device accepts the connection, sends its connect-time snapshot, then goes
quiet". Earlier ledger entries (0030, 0034) read the exact-40 s median as a property of the
firmware; it isn't, and that misreading sent at least one investigation the wrong way.

**⇒ ROOT CAUSE FOUND AND FIXED (fw 1.6.1, 2026-08-24, ledger shq-suite-0038).** The diagnostics
answered it on the actron twin in 20 h: arduinoWebSockets does **blocking socket I/O bounded by
`WEBSOCKETS_TCP_TIMEOUT` (default 5000 ms)**, so one zombie client stalled the main loop for
multiples of 5 s per pass (this fleet mostly escapes because the dirty-flag design keeps the bus
task off the sockets and it pushes far less payload — but `somfy_sdn_06` still hit a 50 s stall),
late pongs made `enableHeartbeat`'s reaper evict the healthy HA client, and reconnect churn
compounded it. fw 1.6.1 = **`-D WEBSOCKETS_TCP_TIMEOUT=500`** in `platformio.ini` (bounds any
stall well under the 5 s pong deadline) + a `diag::tick()` wrap guard (`hb_age` ≥ 0x80000000 is a
wrapped future-stamp read, clamped to 0, never latched as a `heartbeat_stall`).
`WEBSOCKETS_SERVER_CLIENT_MAX` deliberately stays 5 — a smaller cap interacts badly with the
wedge watchdog (5 min at cap ⇒ reboot) and refuses debug clients during zombie windows. The
actron twin additionally ported THIS firmware's dirty-flag broadcast (its bridge task used to
write sockets directly); the two ws_api designs are architecturally aligned again.

**⇒ fw 1.8.0 (2026-08-28) — the guard closed properly, plus BSSID reporting.** Three changes on
top of 1.7.x, all twins with actron: (1) `reapStalled()` now runs **BEFORE** `server.loop()` —
`enableHeartbeat`'s ping is emitted from inside `loop()` and writes to the socket directly,
bypassing the guard, so a blocked slot still present when the library ran cost the full ~10 s core
write (this was the residual 10,016 ms stall); (2) `WS_STALL_REAP_MS` **10 s → 3 s**, comfortably
under the 15 s ping interval so a dead socket is reaped several times over before the library can
touch it, and still far above any transient full send buffer (raised to **30 s** in fw 1.14.0 once
the library ping — the reason for the constraint — was gone); (3) `sendWritableTXT()` guards the
per-client paths too — the connect-time snapshot and the diag backlog are the largest frames this
firmware emits and go to a client whose socket health is unknown.

**Also new in 1.8.0: the firmware reports its own AP.** `bssid=` and `roams=` in `/stats` and
`/diag`, `bssid`/`wifi_roams`/`skipped_writes` in the health push (HA sensors in `somfy_sdn`
1.7.0), and an `ap_change` diag event on every roam. This exists because the flap question had
become "is this a device fault or an AP fault?" and that was unanswerable — the firmware never
reported its association. **Read the association from the STATION, never the UniFi controller
client list** (wiki `estate/shq-network.md`: the controller has been observed disagreeing).

**⇒ fw 1.9.0 (2026-08-31) — minimum client age before reap.** The 3 s stall grace introduced in
1.8.0 was too aggressive: it killed sockets only ~6 s old that had never received a frame
(`ws_stall_reap ... life=6595ms rx=0`), i.e. a coordinator still settling rather than a dead peer,
which just made HA reconnect into the same trap — the flap rate on the two affected controllers
doubled. `WS_REAP_MIN_AGE_MS` (10 s) now makes a young client ineligible for reaping, counted as
`deferred_reaps` (`deferred=` in /stats, plus an HA sensor). At the time the value had to stay
under the 15 s ping interval — the library's unguarded ping — which is why raising the stall grace
instead would have been wrong; fw 1.14.0 removed that constraint altogether (see "WS liveness").

**⇒ SECOND MECHANISM, FIXED IN fw 1.7.0 (2026-08-27).** The 1.6.1 timeout bound worked — 8 of 12
controllers fell to a 806-1629 ms worst stall with zero pong timeouts — but `somfy_sdn_06`
(living room back, 70 unavailable events post-flash) and `_04` (living room left, 31) kept a
**~50,05x ms** stall, matching the actron to within 4 ms. Root cause is the Arduino core, not this
firmware: `NetworkClient::write()` blocks ~10 s per write to a peer that stopped reading
(`WIFI_CLIENT_MAX_WRITE_RETRY` x `WIFI_CLIENT_SELECT_TIMEOUT_US`), unreachable by any build flag.
Fixed with **`src/ws_guard.{h,cpp}`** (twin of actron's — keep in step): zero-timeout `select()`
writability poll before every write, `broadcastWritableTXT()` in place of `broadcastTXT()`, and a
reaper that drops a socket unwritable for `WS_STALL_REAP_MS`. New `ws_stall_reap` diag event and
`stall_reaps` counter. See `actron-sniffer/CLAUDE.md` for the full derivation.

A 48-entry RAM ring (~3 kB). Each record carries the event, an inferred reason, and the machine's
condition at capture: free heap, largest allocatable block, spare lwIP sockets, worst main-loop and
HTTP-pump stall since the previous record, RSSI, client count.

**Events:** `boot` (with `esp_reset_reason()`), `ws_connect`, `ws_disconnect`, `ws_error`,
`ws_at_cap`, `loop_stall`, `heap_low`, `socket_low`, `wifi_down`/`wifi_up`, `clock_glitch`,
`heartbeat_stall`, `ws_stall_reap` (1.7.0), `ap_change` (1.8.0), `net_recover` (1.11.0),
`ws_evict` (1.14.0 — the liveness policy dropped a silent client; `value` = silence ms).

**Disconnect reasons**, inferred conservatively — `unclassified` beats a confident wrong answer,
and the raw evidence rides along so a verdict can be re-judged from history without reflashing
twelve controllers:

| Reason | Inferred when |
|--------|---------------|
| `pong_timeout` | **observed** (1.14.0): the liveness policy evicted it — nothing inbound for 45 s (120 s while retransmitting). Before 1.14.0 this was *inferred* from a pong ≥ 20 s old |
| `stall_reap` | **observed** (1.14.0): the write-guard dropped it for staying unwritable 30 s |
| `peer_close` | not evicted by us, and a pong landed within the last ping cycle or inbound traffic within 10 s ⇒ the far end closed it |
| `transport_error` | a `WStype_ERROR` arrived for that slot in the preceding 2 s |
| `unclassified` | quiet socket, not evicted by us — nothing to pin it on |

**Spare sockets are measured, not guessed:** `probeSockets()` asks lwIP for sockets until it
refuses (up to 3) and closes them again, re-probed at the instant of every disconnect. `sock=0`
means the pool HTTP, WS, OTA and mDNS all share is exhausted.

**Loop phase timing:** `loop()` times `ArduinoOTA.handle()`, `http_api::loop()` and
`ws_api::loop()` separately, so a `loop_stall` record says *which* phase held the loop. `tick()`
runs every iteration but rate-limits its body to 1 Hz — `ESP.getFreeHeap()` takes a heap lock.

**`clock_glitch` counts `clk_word` + `clk_rebase` + `clk_jump`, never `clk_back` — but the old
reason for that was wrong, and it cost a diagnosis.** Ledger shq-suite-0039 concluded `clk_back`
was a meaningless artifact of the lock-free filter racing the bus task, measuring "25.4 million
backward reads on Bed 4 over four days (~72/s) with a perfectly healthy clock", and advised
ignoring it. A fleet sweep on 2026-09-01 measured the true healthy floor: **192 to 853 reads over
four days — about 0.002/s — with Bed 4 itself now reading 192 on the same code.** That 25.4 M was a
device whose clock was pinned at the time, and 0039 even notes it was flapping when measured.
Corrected by shq-suite-0041, which supersedes it.

`clk_back` is excluded here because it is the RAW symptom and would fire a record per tick during a
fault; `clk_word` and `clk_rebase` say what the filter actually *did* about it, which is the part
worth a ring entry. But `clk_back` is the cheapest fingerprint we have of a clock pinned right now
— **judge it by rate, not by total**, and the HA sensor's history is what makes that visible.

**Delivery — three surfaces, and the backlog is the load-bearing one:**
1. Live over WS as `{"type":"diag","event":{…}}`, **polled** from `ws_api::loop()` (max 4/iteration)
   rather than pushed from a callback — every record originates inside the WS library's own event
   dispatch, and emitting from there would re-enter the server mid-iteration.
2. The last 12 records replayed to every client on connect — **a socket can never be told about
   its own death**, so this is the only way HA learns why the previous session ended. Since
   fw 1.14.0 a per-client cursor delivers them through the same `diag` frames as (1), two per
   loop pass; ≤ 1.13.0 sent one `{"type":"diag_backlog","events":[…]}` frame, ~2.6 kB, which
   the pcap showed lost on every attempt on the affected boards. Records are deliberately
   re-sent; HA de-duplicates on `seq`.
3. `{"type":"health"|"health_ws"|"health_net","data":{…}}` every 30 s, a second apart (one
   `health` frame ≤ 1.13.0), plus `GET /diag` (text) and `GET /diag.json`.

`/stats` gained `sock` `loop_max` `http_max` `stalls` `pongto` `peerclose` `txerr` `diag_seq`.

**Do not diagnose these controllers by polling their HTTP server.** On the actron twin, `GET
/stats` every 15 s quadrupled the flap rate and drew heap down 24 k — HTTP and WS share the socket
pool and the main loop. Read `/diag` once, or take it off the WS push, which is what the
`somfy_sdn` HA component's diagnostic sensors now do continuously without touching the device.

### WS liveness (`src/ws_liveness.{h,cpp}` + `src/ws_guard.{h,cpp}`, fw 1.14.0) — why a WS ping/pong cannot measure liveness on a retransmitting TCP flow

**Twins — keep both files byte-identical with `actron-sniffer/src/`.** The policy is pure and
host-tested (`test/test_ws_liveness`, 8 cases); the guard is the glue.

**What the wire showed (ledger shq-suite-0046).** On Living Back and Living Left, and only
there, uplink WiFi frames of ~700 B and up are lost in bursts of 7-30 s while small frames get
through (0/313 lost at ~330 B, 8/65 at ~720 B, 2/4 at >= 1300 B). TCP delivers in order, so ONE
lost segment blocks everything queued behind it: lwIP retransmits from the head on a doubling
ladder (measured 1.2, 2.4, 4.8, 9.6, 19.2 s — cumulative 1.2 / 3.6 / 8.4 / 18.0 / 37.2 / 75.6 s)
and ignores HA's SACK. During a hole the 2 B ping we wrote after the lost frame, the 6 B pong we
owe HA and the 330 B state push all *reach HA's kernel* and sit in its out-of-order queue where
the application cannot see them; HA's own pings meanwhile arrive here fine (downlink is clean)
and we ACK them at the TCP layer. So a pong is evidence of a working **uplink**, not of a living
**peer**, and its absence says "the head segment has not landed yet" — a statement about the
retransmit ladder, not about HA. The library's `enableHeartbeat(15000, 5000, 2)` did not know
that: `handleHBTimeout` re-pings immediately after a miss, so it evicted a client **10 s after
the first unanswered ping** — after only three retransmissions (+1.2, +3.6, +8.4 s) of a segment
lwIP would have kept retrying for minutes. 42 recorded sessions died at `pong_age = 25.0 s`
exactly, and HA (which tolerates 40 s) was never even the binding constraint.

**The policy (`ws_liveness::judge`)**, per client, from three facts: SILENCE = time since the
last inbound frame of *any* kind (pong, ping or text — HA's 20 s pings keep a live HA fresh
through a hole); RETRANSMITTING = the last `tcpsnap` sample has `nrtx > 0 && unacked > 0`;
UNWRITABLE = `select()` says a write would block, which on this build means more than
`TCP_SNDLOWAT` = 2873 B (min(max(SND_BUF/2, 2·MSS+1), SND_BUF−1)) is stuck unacknowledged.

| Constant | Value | Derivation (`static_assert`ed in `ws_liveness.h`) |
|----------|-------|-----------------------------------------------------|
| `PING_INTERVAL_MS` | 15 s | unchanged; two of our pings per liveness window |
| `LIVENESS_MS` | 45 s | evict on silence in BOTH directions — above HA's 40 s (20 s ping + 20 s pong timeout) so HA gives up first and sends a close frame the clean downlink delivers; above the 5th retransmission (37.2 s) |
| `LIVENESS_HARD_MS` | 120 s | evict a silent peer even while lwIP is still retransmitting — above the 6th retransmission (75.6 s); a peer that vanished mid-hole cannot hold a slot for the full MAXRTX (12) ladder |
| `STALL_REAP_MS` | 30 s | unwritable this long ⇒ reap (loop protection, outranks the silence rule) — above the 4th retransmission (18 s). Was 3 s in fw 1.8.0 **only because the library wrote its ping unguarded inside `loop()`** and a blocked slot cost a ~10 s core write; with the library heartbeat off and our ping routed through the guard, the only unguarded writes left are the library's replies to a peer PING/CLOSE inside `loop()`, so an unwritable slot can still cost ~10 s per HA ping (20 s) until reaped — bounded, and only reachable once > TCP_SNDLOWAT (2.9 kB) is stuck |
| `MIN_AGE_MS` | 10 s | never judge a younger client (fw 1.9.0's lesson); now implied by every deadline above, kept as an invariant |
| `FRAME_BUDGET_BYTES` | 600 | the largest hot-path frame; the guard counts anything over it as `big_frames`, which must read 0 |

The library's `enableHeartbeat()` is **not called**. `GuardedWebSocketsServer::sendPings()`
emits the 15 s ping only when the socket is writable *and* not retransmitting (a ping queued
behind a hole only lengthens the tail segment lwIP coalesces new data into); `judge()` runs
before `server.loop()` and evicts with a proper close frame (1000) when the socket is writable,
or `stop()` when it is not. Evictions are **observed, not inferred**: the guard calls
`diag::noteWsEvict` (new `ws_evict` record, `value` = silence ms, live pcb attached) or
`noteWsStallReap`, and marks the slot so the following `ws_disconnect` is classified
`pong_timeout` / `stall_reap` from fact. The old "last pong ≥ 20 s ⇒ our reaper" inference is
gone.

**Frame sizes are the other half.** Telemetry (`diag` records, the health frames) goes through
`sendTelemetryTXT()`, which additionally holds off a retransmitting pcb (`deferred_telemetry`);
state pushes stay on `broadcastWritableTXT()` because availability rides on them and they are
~330 B. The diag backlog is a **record per frame** from a per-client cursor (one frame per
`DIAG_DRAIN_SPACING_MS` = 250 ms per client — NOT per loop pass: `CONFIG_LWIP_TCP_OVERSIZE_MSS`
makes lwIP fill the waiting unsent segment with later writes, so a burst of small frames inside
one RTT still leaves the device as 1436 B segments; cursor only advances on a successful queue)
instead of one 2.6 kB
`diag_backlog` frame — that frame crossed `TCP_SNDLOWAT` on its own, which is why the fresh
socket read *unwritable from birth* in signature (B) and why the churn was self-perpetuating.
The ~1.3 kB health push is now three frames a second apart — `health` (vitals + WS counters +
fault + fw), `health_ws` (guard/liveness counters + lwIP's view of the HA socket), `health_net`
(netwatch + MAC transmit + PHY/protocol) — merged by HA; `diag::healthToJson()` (all three in
one) is for `/diag.json` only. New `health_ws` keys: `liveness_evicts`, `liveness_extended`
(holes ridden out instead of evicted), `deferred_telemetry`, `pings`, `ping_skips`,
`big_frames`.

**Reading it in HA.** `sensor.<controller>_pong_timeouts` now counts liveness evictions; a
climbing `liveness_extended` with zero evictions is the policy doing exactly what it is for;
`ws_evict` logbook lines carry the pcb (`tcp_nrtx`, `tcp_unacked`) at the instant of the verdict.

### MAC transmit telemetry (`src/txstats.{h,cpp}` + `src/tcpsnap.{h,cpp}`, fw 1.12.0)

**⚠️ fw 1.12.0 boot-looped the canary (ledger shq-suite-0047).** `esp_wifi_get_tx_statistics()`'s
`tx_fail` argument is an ARRAY of `TEST_TX_FAIL_MAX` (6) structs — the driver memcpy()s 984 bytes
into it, one 164 B block per `esp_test_tx_fail_state_t` — but the private prototype declares a bare
pointer and the header comment says "164 bytes". Passing a single struct overran `readAll()`'s stack
frame by 820 bytes and zeroed the return address on the very first `loop()` pass (the netwatch tick
fires immediately because its stamp starts at 0), so the device panicked ~50 ms after `ws_api::begin`,
wrote a core dump, rebooted, and repeated every 7-9 s. 1.12.1 passes `static f[TEST_TX_FAIL_MAX]`
(static also keeps 1.1 kB off loopTask's 8 kB stack), sums the failure counters over states 1..5
(state 0 is `TEST_TX_SUCCESS`), and `static_assert`s both struct sizes. The IDF's own examples are the
only public statement of the array contract; the library disassembly is the proof.

**Instrumentation only — no behaviour change.** Written 2026-09-05 as the baseline half of an A/B
for the long-frame uplink-loss finding: two of twelve controllers on one AP radio (`_06` Living
Back ~50/day, `_04` Living Left ~25/day) churn their HA WS session while ten identical neighbours
do not, and a live capture put the loss in the **uplink** as a function of **frame length** —
731 B and 1436 B segments lost four retransmissions in a row while 2-332 B frames in the same
burst all arrived, and the 64 B gateway ICMP never failed. Every layer above the MAC had been
measured clean (socket pool, heap, clock, lwIP, the write-guard); the MAC itself never had. This
firmware makes it report.

- **What is read.** The ESP32-C6 driver keeps per-access-category transmit statistics behind
  `esp_wifi_enable_tx_statistics()` / `esp_wifi_get_tx_statistics()` (`esp_test_tx_statistics_t`
  + the failure-state matrix `esp_test_tx_fail_statistics_t`). All four ACs are enabled on the
  first link-up (re-checked every tick against the driver's own `ena_acibitmap`, so a
  re-association cannot silently drop them), read once a second from the same 1 Hz tick as the
  netwatch policy, and **explicitly cleared after each read** so the code does not depend on
  whether the getter clears — a failed clear is handled by differencing the next read. Also read:
  `esp_wifi_sta_get_negotiated_phymode()` + `esp_wifi_sta_get_ap_info()` into one string,
  `phy="HE20 ch6 bw20 bgnax"` (negotiated mode, primary channel, bandwidth, AP PHY set, wps/ftm).
- **Where it surfaces.** `/stats`: `tx_ok tx_retry tx_tbretry tx_to tx_coll tx_nomem tx_fail
  tx_en phy ch`. `/stats.json`: `wifi_tx{…}` + `phy` + `channel`. Health push: lifetime totals
  (`tx_ok tx_retry tx_retry_edca tx_tbretry tx_tb tx_ack tx_ba tx_to tx_coll tx_nomem tx_fail
  tx_fail_to tx_err tx_rtt_max_us tx_en tx_samples phy channel`) plus deltas since the previous
  push (`tx_ok_d tx_retry_d tx_to_d tx_fail_d`) and the first connected client's pcb (`tcp_client`
  + `tcp_*`). **Every diag record** carries `tx_ok tx_retry tx_to tx_fail` as deltas since the
  *previous record* (u16, saturating), so a `ws_disconnect`/`ws_stall_reap` shows the MAC picture
  of its run-up; those two also carry the pcb: `tcp_state tcp_nrtx tcp_rto_ms tcp_cwnd tcp_snd_buf
  tcp_snd_wnd tcp_qlen tcp_unacked tcp_dupacks tcp_flags` (`tcp_flags & 0x0800` = `TF_RTO`, an RTO
  has fired). HA (`somfy_sdn` 1.11.0): `sensor.<controller>_wifi_frames_acknowledged`,
  `_wifi_tx_retries`, `_wifi_tb_retries`, `_wifi_ack_timeouts`, `_wifi_collisions`,
  `_wifi_tx_buffer_starvation`, `_wifi_tx_failures`, `_wifi_phy_mode`; the logbook renders the
  MAC/TCP tail on disconnect and stall-reap lines.
- **How to read it.** `tx_en` must be 15 (all four ACs) — 0 means the enable never took and every
  counter is a meaningless zero. Compare `tx_retry`/`tx_to` *per acknowledged frame* between the
  churning pair and a clean neighbour, and look at the `ws_stall_reap` records on the churners:
  a socket unwritable from birth with `tcp_nrtx` climbing and `tcp_unacked` at a full window is
  the uplink not getting through; `tcp_nrtx=0` with an empty window is something else. If
  `phy` differs between the pair and the ten (HE vs HT), that is the first lever for the B side.
- **Caveats.** `esp_wifi_get_tx_statistics()` / `esp_wifi_clr_tx_statistics()` are exported by
  the prebuilt `libnet80211.a` but declared only in the **private** header
  `esp_private/esp_wifi_he_private.h`, and the sdkconfig option that would enable them at init
  (`CONFIG_ESP_WIFI_ENABLE_WIFI_TX_STATS`) is **unset** in the Arduino libs — hence the runtime
  enable. The struct layouts come from `esp_private/esp_wifi_he_types_private.h` in the same
  package, so they match the library they ship with; **re-check them on any pioarduino platform
  bump.** The pcb walk relies on `CONFIG_LWIP_TCPIP_CORE_LOCKING=y` (it is) and `tcp_active_pcbs`
  being exported (it is). A `ws_disconnect` record's pcb is the last 1 Hz sample, up to a second
  stale, because the library closes the socket before it reports; a `ws_stall_reap` pcb is live.
  Cost: ~8 driver ioctls/s (the same path as `WiFi.RSSI()`), +1.8 kB RAM, +6.9 kB flash.
- **1.13.0 corrections, from the live 1.12.1 units.** (1) **No `tcp_*` key ever landed:** the
  Arduino core's `NetworkServer::begin()` opens `socket(AF_INET6)` bound to `in6addr_any`
  under `CONFIG_LWIP_IPV6` (on in the prebuilt C6 libs), so `lwip_getpeername()` on an accepted
  socket returns an IPv4-MAPPED IPv6 sockaddr (`::ffff:a.b.c.d`, family `AF_INET6`) — the same
  thing the core's own `NetworkClient::remoteIP()` unmaps — while the pcb's `remote_ip` stays
  v4-typed. `tcpsnap::capture()` rejected `AF_INET6` and returned false before walking the list.
  Now unmapped (`tcpsnap::mappedV4()`, host-tested), and `tcp_miss` in the health push counts
  captures that found no pcb so a silent miss can never hide again. (2) **`tx_rtt_max_us` was
  not microseconds:** a 10-minute-old unit reported 545,967,505. The driver's `tx_seq_max_rtt`
  is documented only as "rtt of a sequence number containing the time of retries" with no unit;
  it is dropped from `/stats.json`, the health push and the records, and kept raw in the
  accumulator until its meaning is known.

### WiFi protocol A/B (`src/wifi_proto.h`, fw 1.13.0, ledger shq-suite-0046)

**Why.** The wire capture settled the *mechanism* of the Living Back / Living Left churn (ledger
shq-suite-0046): on those two boards, and only those two, uplink TCP segments of ~700 B and up
are lost in the air on four consecutive retransmissions while the 2-332 B frames sent in the
same burst arrive every time; downlink and the 64 B gateway ICMP never fail. Every layer above
the radio has been measured clean. The ranked physical suspects all sit on the **11ax (HE20)
uplink** the boards negotiate by default against the UniFi U6 Enterprise 2.4 GHz radio (HE
TB-PPDU / sounding, high-MCS A-MPDU PER from a marginal front end or crystal, a stale partial
PHY calibration). The cheapest falsifiable experiment is to take **one** churning unit off 11ax
and, if that is not enough, off 11n, with its clean neighbour (Living Right, same room, same
radio) left on the default as the control — and to do that without a reflash so the whole fleet
runs one image and any unit can be switched, or switched back, from HA.

**The knob.** `wifi_proto` in NVS (namespace `somfy`, key `wifi_proto`), read once at boot:

| Value | Bitmap passed to `esp_wifi_set_protocol(WIFI_IF_STA, …)` | Effect |
|-------|------|--------|
| `bgnax` (default) | `esp_wifi_set_protocol(B\|G\|N\|AX)` — **written explicitly since 1.14.1** | the driver's own post-init default, B\|G\|N\|AX, HE20 |
| `bgn` | `11B\|11G\|11N` | HT20, no HE: kills TB-PPDU / HE sounding while keeping A-MPDU + MCS |
| `bg` | `11B\|11G` | legacy OFDM/DSSS only: no A-MPDU, no MCS, 54 Mb/s max |

Applied by `applyProto()` **after `WiFi.mode(WIFI_STA)` and before every `WiFi.begin()`** — the
boot connect in `tryConnect()`, `reassociate()` (manual `/reconnect`, `reconnect_wifi`, netwatch)
and the link-retry loop in `serviceWatchdogs()`. Where in the core the bitmap may be touched was
read, not guessed: `WiFiGenericClass::mode()` (WiFiGeneric.cpp) returns early when the mode is
unchanged, and otherwise only rewrites the bitmap in its long-range branch — with `_long_range`
off it calls `_wifi_is_lr_enabled()` and rewrites to `WIFI_PROTOCOL_DEFAULT` (B\|G\|N\|AX on the C6)
**only if the current bitmap equals LR**. `STA.begin()/connect()` never touch it. So the 11ax
default is the IDF driver's, the core never re-applies it over ours, and the value survives
disconnect/connect; it would only be lost on `WiFi.mode(WIFI_OFF)` (deinit), which this firmware
never calls. Calling it *before* `WiFi.mode()` would fail with `ESP_ERR_WIFI_NOT_INIT`, which is
why it sits after it. **A live protocol change needs a fresh WiFi *init*, not merely a fresh association** —
measured on the canary: `POST /wifiproto?set=bgn` followed by the 1.13.0 re-associate left it
negotiating HE20 (uptime continuous), a reboot with the same NVS value came up HT20. So since
fw 1.14.0 every setter persists, replies, then `noteReboot("wifiproto")` (the actron twin
`ESP.restart()`s — zones off first there).

**Surfaces.** `GET/POST /wifiproto`, WS `set_wifi_proto{proto}`, console `proto [bgnax|bgn|bg]`
(bare `proto` prints the current value), HA `select.<controller>_wifi_protocol`. Reported as
`proto=` in `/stats`, `proto` in `/stats.json` and in the health push (HA
`sensor.<controller>_wifi_protocol`, diagnostic). **Read `proto` against `phy`:** `proto` is what
the station was allowed to negotiate, `phy` (`HE20 …` / `HT20 …` / `11G …`) is what it did — if
`phy` still says `HE20` after `bgn`, the driver did not honour the call and the A/B is invalid.

**How to run it.** (1) Baseline both the churning unit and its control on `bgnax` for a day with
`sensor.*_websocket_disconnects` / `_pong_timeouts` / `_wifi_tx_retries` / `_wifi_ack_timeouts` in
the recorder. (2) Flip the churning unit only: `select.<unit>_wifi_protocol` → `bgn` (or
`curl -X POST http://<ip>/wifiproto?set=bgn`); the unit reboots (~10 s, RAM diag ring lost);
confirm `phy` moved to `HT20`. (3) Compare the
churn rate over the next day against its own baseline and the control. (4) If unchanged, `bg`
— if the loss persists with no A-MPDU and no MCS, the radio path itself is the suspect and
`POST /phycal?erase=1` is the next lever. (5) Put it back with `bgnax`. Never flip the control.

**`/phycal`** is HTTP-only on purpose: it reboots, so it destroys the RAM-only diagnostic ring,
and it is a one-shot remedy, not a setting — nothing in HA should be able to press it by accident.

## Open items (need a Somfy Set Pro capture — SPEC §13)

Best-known guesses are implemented and flagged `CONFIRM` in `src/sdn.h`; confirm on the bench
with `/send` + LISTEN-mode sniffing:

1. **SET_MOTOR_LIMITS** `{fn, direction, param}` layout + direction enum (up vs down).
2. ~~**SET_MOTOR_DIRECTION** `{0x00/0x01}` = standard/reversed~~ — **CONFIRMED on hardware**
   (writes ACK + read back). Still unknown: whether a direction change forces a re-calibration.
3. ~~**SET_FACTORY_DEFAULT** payload~~ — **RESOLVED**: 1-byte scope (`RESET_*` in sdn.h). "Reset
   positions" uses `limits` (`0x11`); `all_settings` (`0x00`) also wipes addr/label. The earlier
   empty payload was a no-op (hardware-confirmed). Verified jog/limits/cover all work on the real
   fixture; reset now sends the scope byte.
4. ~~**POST_MOTOR_LIMITS** byte offsets~~ — **RESOLVED**: up=`[0..1]`, down=`[2..3]` is correct.
   A calibrated motor reads `up_limit=0` (top 0-reference), `down_limit`=travel (e.g. 1039) — the
   only sensible reading; a swapped offset would give a nonsensical `up`=travel.
5. **CTRL_WINK** payload — sent empty; confirm.
6. **Multi-motor discovery collisions** — active discovery is best-effort (simultaneous replies
   collide); prefer configured addresses / passive observation on a populated bus.
