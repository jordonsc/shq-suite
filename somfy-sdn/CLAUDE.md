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
motor (2026-06-05)**. Pure-C++ core host-unit-tested (`pio test -e native`, 22 cases). Full
design rationale: [`SPEC.md`](SPEC.md).

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
| `platformio.ini` | `um_tinyc6` (Arduino, pioarduino C6 platform) + `um_tinyc6_ota` (espota) + `[env:native]` host tests (compiles `sdn.cpp`/`errlog.cpp`/`devices.cpp` only). |
| `src/sdn.{h,cpp}` | **Pure C++, no Arduino deps.** Framing/checksum/inversion + command-payload builders + response parsers + address & HA-inversion helpers. Host-tested. |
| `src/errlog.{h,cpp}` | **Pure C++.** Bounded ring buffer (128) of wire/protocol events + per-class counters. Host-tested. |
| `src/devices.{h,cpp}` | **Pure C++.** Device table keyed by node addr — registration, position/limit application, stall + fault detection, comms-loss sweep. Host-tested. (This is the firmware's state model; there is intentionally no separate `state.cpp` — the table *is* the snapshot, serialised in `ws_api`/`http_api`.) |
| `src/bus.{h,cpp}` | Arduino. The **only** code touching UART1. FreeRTOS task: LISTEN/ACTIVE TX gate, command queue + raw request/response, retry, polling cadence, passive sniffing → device table + sniffer ring + errlog, OTA teardown. |
| `src/ws_api.{h,cpp}` | WebSockets controller API (port 8767). Push `state` snapshots, command/`ack`/`error`, heartbeat. Broadcasts only from the main loop (dirty-flag set by the bus task). **Protocol-level ping/pong with dead-client eviction is enabled in `begin()` (`enableHeartbeat(15000,5000,2)`)** — the app-level state push is a data broadcast, not a liveness probe, so without this a half-open client left by a WiFi blip (no TCP FIN) lingers until lwIP's retransmit timeout (minutes), stalling the WS service loop and blocking new handshakes. That was the root cause of multi-minute HA `unavailable` stretches on weak-signal motors (port-80 HTTP stays responsive throughout, masking it). Fixed in fw 1.1.5. |
| `src/http_api.{h,cpp}` | HTTP debug API (port 80). `/stats /devices /log /errors` (GET) and `/mode /send /discover /move /forget /wifi /reconnect /update /clear` (POST). `/send` is the RE workhorse. |
| `src/version.h` | `SOMFY_FW_VERSION` semver — bump on every flash (see root CLAUDE.md → Versioning). |
| `src/wifi_prov.{h,cpp}` | NVS creds (Arduino `Preferences`), STA connect w/ retries, SoftAP captive portal, GPIO0 button (long = wipe, short = wink all), mDNS, configured-motor loading. **STA connect uses `WIFI_ALL_CHANNEL_SCAN` + `WIFI_CONNECT_AP_BY_SIGNAL`** (not the Arduino-default `WIFI_FAST_SCAN`, which joins the *first* matching BSSID — often a cached distant AP — and sticks there with no roaming), so every (re)connect joins the **strongest** AP for the SSID; a reboot/OTA now lands on the nearest AP. The stack has no live roaming once associated, so `requestReconnectBestAp()` (HTTP `POST /reconnect` / WS `reconnect_wifi` / HA button) forces an on-demand re-scan + reassociate — deferred to `loop()` so the ack flushes before the link drops. Hostname/SoftAP SSID = `somfy-sdn-<XXXX>` where `XXXX` is the last 2 octets of the **STA MAC** (`esp_read_mac(ESP_MAC_WIFI_STA)`), so it matches the device's label/MAC. (Do **not** use `ESP.getEfuseMac() & 0xFFFF` — that's the shared vendor OUI; every TinyC6 came out `4C40`. getEfuseMac also returns the *base* MAC, which differs from the STA MAC on the C6.) |
| `src/main.cpp` | Boot wiring: bus task → WiFi/provisioning → HTTP+WS (when connected). |
| `test/test_sdn`, `test/test_devices` | Unity host tests (22 cases). `pio test -e native`. |
| `WIRING.md` | Parallel-tap wiring (single transceiver, no terminator on a mid-bus tap). |

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

**OTA server reliability (learned the hard way):** detaching the atlas `http.server` over SSH
(`nohup`/`setsid`/`disown`) raced and frequently left it dead when the device pulled. The robust
way is to hold it open with a single live background SSH for the duration of the pull:
`ssh atlas 'cd ~/somfy-ota && exec python3 -m http.server 8088'` (run in background), verify it
serves, `POST /update`, confirm the new `fw=`, then kill the SSH.

## HTTP API (port 80, LAN, all open)

| Endpoint | Purpose |
|----------|---------|
| `GET /` | human-friendly **HTML dashboard** (auto-refreshes every 3 s; polls `/stats.json` + `/devices` client-side) |
| `GET /help` | the old text endpoint listing + status line (RE/curl workflow) |
| `GET /stats` | status line, text (mode, devices, counters, fw, rssi, ip) — consumed by the OTA flash scripts |
| `GET /stats.json` | controller status as JSON (fw, build, mode, hostname, ip, mac, ssid, rssi, uptime, heap, counters) — drives the dashboard |
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
| `POST /update?url=` | HTTP-pull OTA |
| `POST /clear` | reset ring buffers + counters |

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
(alongside limits) so it reports rather than sitting unknown. Commands (envelope
`{type:"command", command, addr, id}`):
`open` `close` `stop` `set_position{position=HA%}` `jog{direction,duration}` (timed CTRL_MOVE,
commissioning) `move_steps{direction,pulses}` (post-cal) `set_top_limit` `set_bottom_limit`
`set_bottom_limit_pulses{pulses}` (absolute, specified_position) `set_direction{reversed}`
`reset` (limits) `identify` `forget` (all carry `addr`); admin:
`set_mode{mode}` `rediscover` `reconnect_wifi` (re-scan + reassociate to the strongest AP; bounces
the link, so this WS connection drops and the coordinator reconnects). Replies:
`{type:"ack",id}` / `{type:"error",id,message}`.
`ack` = command accepted into the queue — there is **no optimistic state**; motor-confirmed
state arrives via the next snapshot.

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
limits_not_set**, `0x23` ip_not_set, `0x24` out_of_range, `0xFF` busy. Observed on this
uncalibrated motor: `0x21`/`0x22`/`0x11` when attempting MoveOf or set-limit — i.e. "no limits /
no position reference yet", cleared once commissioned via CTRL_MOVE + limit-set.

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
