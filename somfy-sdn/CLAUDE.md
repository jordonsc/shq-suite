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
| `src/ws_api.{h,cpp}` | WebSockets controller API (port 8767). Push `state` snapshots, command/`ack`/`error`, heartbeat. Broadcasts only from the main loop (dirty-flag set by the bus task). **Protocol-level ping/pong with dead-client eviction is enabled in `begin()` (`enableHeartbeat(15000,5000,2)`)** — the app-level state push is a data broadcast, not a liveness probe, so without this a half-open client left by a WiFi blip (no TCP FIN) lingers until lwIP's retransmit timeout (minutes), stalling the WS service loop and blocking new handshakes. That was the root cause of multi-minute HA `unavailable` stretches on weak-signal motors (port-80 HTTP stays responsive throughout, masking it). Fixed in fw 1.1.5. **Wedge watchdog (fw 1.3.0):** the heartbeat only evicts *non-responsive* clients — it can't help when all `WEBSOCKETS_SERVER_CLIENT_MAX` (5) slots fill with *live* zombies (a client leaking duplicate connections, each kept alive by its own keepalive). At capacity the library refuses every new handshake (accept→drop, no HTTP response) and the device looks dead to HA while HTTP/the bus stay healthy. A controller only ever has one legitimate HA coordinator, so `loop()` reboots if `connectedClients()` stays at the cap continuously for `WEDGE_REBOOT_MS` (5 min); any drop below the cap resets the timer (reboots via `wifi_prov::noteReboot("ws-wedge")` as of 1.5.0, so the cause survives into the next boot's `/stats`). This is the backstop for an HA-side connection leak fixed at source by the coordinator's close-before-reconnect (`somfy_sdn` 1.4.1) — it first wedged `bed_1_blinds_left`'s controller on 2026-06-14. **Silent-socket telemetry (fw 1.5.0, ledger shq-suite-0022):** lifetime `onEvent` counters `ws_conn`/`ws_disc`/`ws_err` (ported from actron-sniffer) surfaced in `/stats` + `/stats.json` — a growing `ws_conn − ws_disc` gap means sockets are dying without the WS library seeing a DISCONNECTED (the silent-death signature of the Actron/somfy ~5-day-fuse failure). |
| `src/http_api.{h,cpp}` | HTTP debug API (port 80). `/stats /stats.json /devices /log /errors` (GET) and `/mode /send /discover /move /forget /wifi /reconnect /update /clear` (POST). `/send` is the RE workhorse. `/update` carries the **OTA app-guard** (see Device identity below). |
| `src/app_desc.cpp` | **Native `esp_app_desc` override (Option A).** A strong `extern "C"` `esp_app_desc` in section `.rodata_desc` shadows the prebuilt Arduino one (`project_name="arduino-lib-builder"`), so the image's native descriptor reports `project_name="somfy-sdn"` + the real `SOMFY_FW_VERSION`. Read by `esp_ota_get_partition_description()` in the OTA guard and by `esptool image_info`. See "Device identity". |
| `src/version.h` | `SOMFY_FW_VERSION` semver — bump on every flash (see root CLAUDE.md → Versioning). |
| `src/wifi_prov.{h,cpp}` | NVS creds (Arduino `Preferences`), STA connect w/ retries, SoftAP captive portal, GPIO0 button (long = wipe, short = wink all), mDNS, configured-motor loading. **WiFi hardening (fw 1.5.0, ledger shq-suite-0022 — three in-wall controllers wedged off-network after infra outages, needing a breaker power-cycle):** after the one-shot boot connect the firmware previously relied entirely on the Arduino stack's implicit auto-reconnect; when that got stuck (the known ESP32 glitch class after an AP reboot/rekey/channel change) the device was stranded forever. Now layered: (1) **active link-retry** — after 20 s of continuous downtime (auto-reconnect gets the easy cases first), force a full `WiFi.disconnect()` + `begin()` every 30 s, resetting a stuck association state machine and re-applying the all-channel strongest-AP scan; (2) **WiFi-death reboot watchdog** — STA link down continuously for 5 min ⇒ `noteReboot("wifi-dead")` (backstop if even re-begin can't recover); (3) **mDNS re-announce on every (re)association** (`GOT_IP` event → `MDNS.end()` + restart from `loop()`): a reconnect may land on a NEW IP and HA's zeroconf host-healing only works if the advert re-fires — ESPmDNS's own IP-change behaviour is not dependable; (4) **event telemetry** — `WiFi.onEvent` counts lifetime STA disconnects + last 802.11 reason code, surfaced as `wifi_disc=`/`wifi_reason=` in `/stats` (handler stays minimal; logging happens from `loop()`). **Portal-purgatory retry (fw 1.5.0):** a device that boots while the AP is down (e.g. post-power-outage, AP slower to start) used to fall into the portal and stay there forever despite valid creds; with creds present the portal now reboots to retry STA every 15 min (`noteReboot("portal-retry")`); a creds-less portal never retries. **`noteReboot(reason)`/`bootNote()`:** records the reason for a deliberate self-reboot in NVS; the next boot reads + clears it and surfaces it as `note=` in `/stats` / `boot_note` in `/stats.json` (alongside `reset=` from `esp_reset_reason()`, so power-cycle vs watchdog vs crash is distinguishable per fleet sweep). **STA connect uses `WIFI_ALL_CHANNEL_SCAN` + `WIFI_CONNECT_AP_BY_SIGNAL`** (not the Arduino-default `WIFI_FAST_SCAN`, which joins the *first* matching BSSID — often a cached distant AP — and sticks there with no roaming), so every (re)connect joins the **strongest** AP for the SSID; a reboot/OTA now lands on the nearest AP. The stack has no live roaming once associated, so `requestReconnectBestAp()` (HTTP `POST /reconnect` / WS `reconnect_wifi` / HA button) forces an on-demand re-scan + reassociate — deferred to `loop()` so the ack flushes before the link drops. Hostname/SoftAP SSID = `somfy-sdn-<XXXX>` where `XXXX` is the last 2 octets of the **STA MAC** (`esp_read_mac(ESP_MAC_WIFI_STA)`), so it matches the device's label/MAC. (Do **not** use `ESP.getEfuseMac() & 0xFFFF` — that's the shared vendor OUI; every TinyC6 came out `4C40`. getEfuseMac also returns the *base* MAC, which differs from the STA MAC on the C6.) |
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
| `GET /stats` | status line, text (mode, devices, counters, fw, rssi, ip; **1.5.0 adds** `heap/minheap/maxblk/uptime`, `ws/ws_conn/ws_disc/ws_err`, `wifi_disc/wifi_reason`, `reset=`/`note=` — the silent-socket + link-churn telemetry; note `ws_clients=` was renamed `ws=` for actron parity) — consumed by the OTA flash scripts |
| `GET /stats.json` | controller status as JSON (fw, build, mode, hostname, ip, mac, ssid, rssi, uptime, heap, counters; **1.5.0 adds** `heap_min`, `heap_maxblk`, `ws_conn`/`ws_disc`/`ws_err`, `wifi_disc`/`wifi_reason`, `reset_reason`, `boot_note`) — drives the dashboard |
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
