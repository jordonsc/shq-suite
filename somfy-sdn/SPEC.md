# Somfy SDN Controller — Specification

**Status:** Draft spec (no code yet). Target session deliverable.
**Author context:** Designed as a twin of `actron-sniffer/` — same firmware stack and debugging
ergonomics — but for the **Somfy Digital Network (SDN)** RS485 bus, replacing the Matter path
currently provided by the `omni` app in `/mnt/t/Repos/matter-apps` (`common/features/app_sdn.cpp`).

---

## 1. Rationale

We already run Somfy Sonesse motors via the `omni` ESP-Matter firmware (one motor per device,
exposed as a Matter `WindowCovering`). Matter works but is opaque: when a motor misbehaves
there is **no way to see the wire**, no error history, and no calibration surface — you get a
cover that silently fails. Matter also ties us to a dev/test vendor ID and a heavyweight
commissioning flow.

The Actron MITM controller proved the alternative: a small ESP32 on the bus with an **HTTP
debug API + WebSocket control/push API + a dedicated Home Assistant component**. It is trivial
to observe, drive, and OTA-update, and debugging is immediate. We port that design to Somfy SDN.

**This is NOT a MITM.** Actron required a cut-bus bridge because the NEO is the bus master and
we had to rewrite its frames in flight. SDN is an ordinary multi-drop RS485 bus where any
**tool node** may address motors directly. So this device is a **normal bus participant**: one
transceiver, tapped across A/B, acting as a tool. It defaults to **listen-only** and only
transmits once deliberately enabled (same firmware-gated TX safety as Actron).

### Pros / Cons (carried from the proposal)

| Pros | Cons |
|------|------|
| Debug live on the wire (sniff + raw inject) | Bespoke protocol (no Matter ecosystem auto-discovery) |
| Add arbitrary metrics / calibration controls | We own the firmware + HA component |
| No Matter dev/test vendor ID | — |
| Easy HTTP-pull OTA | — |
| Per-motor error history & comms-loss reporting | — |

---

## 2. Goals / Non-goals

### In scope
- **1:1 port of end-user cover control:** open (to up limit), close (to down limit), stop,
  go-to-position (%). These reuse the *proven* encodings already working in `app_sdn.cpp`.
- **Calibration controls:** manual move by steps/pulses, set top limit, set bottom limit,
  reset positions (factory default), change motor rotation direction.
- **Multiple motors per bus:** a device table keyed by SDN node address; one HA `cover` per
  motor; per-device position / limits / fault / last-seen.
- **Status visibility:** motors detected, wire errors (checksum/framing/timeout/NACK), loss of
  comms with a device, fault/stall state.
- **Debug endpoints:** HTTP API mirroring Actron (`/stats`, `/log`, raw frame inject/sniff),
  plus a bounded **error ring buffer** browsable over HTTP.
- **Passive sniffer mode** — observe an existing controller or **Somfy Set Pro** on the wire.
- **WiFi provisioning** — no baked-in credentials (see §7).
- **HTTP-pull OTA** — same flow as Actron.
- **Home Assistant component** (`somfy_sdn`) — config-flow + push WS coordinator + `CoverEntity`
  per motor + calibration services.

### Out of scope (for now)
- Tilt / venetian angle control (Sonesse roller shades only; payloads support it, deferred).
- Group addressing writes (we address motors individually; group reads may be observed).
- Somfy RTS (radio) — this is wired SDN only.
- Scenes/IP (intermediate position) management beyond reading the IP field.

---

## 3. Architecture

```
┌──────────────────────────────────────────────────────────┐
│                     Home Assistant                        │
│                   (redacted.host:8123)                     │
│   custom_components/somfy_sdn  (config flow + WS coord)    │
│      cover.somfy_<addr>  ×N      services: calibrate/…     │
└───────────────────────────┬──────────────────────────────┘
                            │ ws://<host>:8767  (push state + commands + acks)
                            │ http://<host>/    (debug + OTA + provisioning)
┌───────────────────────────▼──────────────────────────────┐
│            somfy-sdn firmware (ESP32-C6 / TinyC6)          │
│  ┌──────────┐  ┌──────────┐  ┌─────────┐  ┌────────────┐  │
│  │ WS API   │  │ HTTP API │  │  OTA    │  │ provisioning│  │
│  │ (8767)   │  │ debug    │  │ (pull)  │  │ (SoftAP)    │  │
│  └────┬─────┘  └────┬─────┘  └─────────┘  └────────────┘  │
│       └────────┬────┘                                      │
│        ┌───────▼────────┐    device table (per node addr)  │
│        │  SDN bus mgr    │    state / limits / faults / log │
│        │ (FreeRTOS task) │                                  │
│        └───────┬────────┘                                  │
└────────────────┼──────────────────────────────────────────┘
                 │ UART1  GPIO16(TX/DI) / GPIO17(RX/RO)
          ┌──────▼───────┐
          │ TTL→RS485     │  auto-direction (no DE/RE)
          │ transceiver   │
          └──────┬───────┘
                 │ A / B / GND  (tapped onto the multi-drop SDN bus)
        ┌────────┴───────────────────────────┐
        │  motor 01:00:23   motor 01:00:51 …  │
        └────────────────────────────────────┘
```

Mirrors Actron's split: an HTTP API for RE/operator use, a WS API as the runtime surface for
HA, a dedicated FreeRTOS task owning the UART, and HTTP-pull OTA. New vs Actron: a device table
(multi-motor), an error ring buffer, and WiFi provisioning (no baked creds).

---

## 4. Hardware & wiring

Identical class of build to `actron-sniffer`. Existing field devices are **already wired** this
way:

| Part | Notes |
|------|-------|
| Unexpected Maker **TinyC6** | ESP32-C6, USB-C, 3.3 V logic. PlatformIO board `um_tinyc6`. |
| **3.3 V** TTL↔RS485 transceiver | **Auto-direction** (keys driver off UART TX; no DE/RE pin). 3.3 V logic only — never a 5 V MAX485 into the C6. |
| Momentary button on **GPIO0 → GND** | Provisioning / factory trigger (see §7). Already wired. |

**UART1 wiring (matches Actron + existing devices):**
- `GPIO16` → transceiver **DI/TXD** (board "TX")
- `GPIO17` ← transceiver **RO/RXD** (board "RX")
- 3V3 + GND to the transceiver.

**SDN bus tap** (this is a *bus participant*, wired in parallel — do **not** cut the bus):
- transceiver **A** ↔ bus **Data+ / A**
- transceiver **B** ↔ bus **Data− / B**
- transceiver **GND** ↔ bus **GND** (and TinyC6 GND)

Somfy SDN over an RJ-style cable (from `matter-apps/README.md`, T568B colours):

| Pin | Colour | SDN role |
|-----|--------|----------|
| 1 | white/orange | RS485 + / Data+ / A |
| 2 | orange | RS485 − / Data− / B |
| 8 | brown | RS485 ground |

**Termination:** on a mid-bus parallel tap, do **not** add the 120 Ω A–B terminator. The bus is
terminated at its physical ends.

**Power:** USB-C charger / bus-derived 5 V buck. As with Actron, do not run an active RS485
driver off a current-limited host USB port if it browns out.

---

## 5. SDN protocol reference

Consolidated from the working `app_sdn.cpp` implementation and the public references in §15.
Marked **⚠ confirm** where a calibration payload still needs a Set Pro capture (§13).

### 5.1 Line parameters
- **4800 baud, 8 data bits, ODD parity, 1 stop bit** (`8-O-1`).
- Half-duplex. Min inter-message gap ≈ **25 ms**; reply timeout ≈ **280 ms** (the working code
  uses a conservative 1000 ms first-byte timeout + per-byte follow-up — keep that).
- `uart_wait_tx_done` before listening so the auto-direction transceiver has flipped back to RX.

### 5.2 Frame structure
Two representations:
- **Raw frame** — logical field values.
- **Bus frame** — every byte **except the 2-byte checksum** is **bitwise inverted** (`~b`, i.e.
  `0xFF − b`). The checksum is appended **un-inverted**.

```
Raw:  [MSG_ID(1)][LEN|flags(1)][NET(1)][SRC_ADDR(3)][DST_ADDR(3)][DATA(0..n)][CKSUM(2)]
                   bit7 = directed flag (set = expects ACK/NACK)
                   bits0-6 = total frame length incl. checksum
```

- **Checksum** = 16-bit sum of all **bus (inverted)** bytes preceding it, stored **big-endian**,
  appended un-inverted. Validate on RX by summing the inverted bytes before un-inverting.
- **Network byte (raw):** `0xF9` tool→motor (directed), `0xF0` broadcast, `0x9F` motor→tool.
- **Addresses** are 3 bytes, **byte-reversed** from the display form: display `01:00:00`
  → raw `{0x00,0x00,0x01}`. Our tool source address is display `01:00:00` (raw `{00,00,01}`),
  as in the current code. Broadcast dest = `{0xFF,0xFF,0xFF}`.
- **Data multi-byte values are little-endian.** Checksum is the only big-endian field.

> These are exactly the rules already implemented in `sdn_build_frame` / `sdn_parse_frame`.
> Port them verbatim; they are field-proven.

### 5.3 Message ID table (raw IDs)

| ID | Name | Dir | Use |
|----|------|-----|-----|
| 0x01 | CTRL_MOVE | →motor | (forced) move up/down — we prefer MOVETO |
| 0x02 | CTRL_STOP | →motor | stop |
| 0x03 | CTRL_MOVETO | →motor | move to limit / IP / percentage |
| 0x04 | CTRL_MOVEOF | →motor | **relative move** (pulses / 10 ms) — *move-by-steps* |
| 0x0C / 0x0D | GET / POST_MOTOR_POSITION | | read position |
| 0x0E / 0x0F | GET / POST_MOTOR_STATUS | | read motor status |
| 0x11 | **SET_MOTOR_LIMITS** | →motor | **set top/bottom limit** |
| 0x12 | **SET_MOTOR_DIRECTION** | →motor | **rotation direction** |
| 0x13 | SET_MOTOR_ROLLING_SPEED | →motor | speed (deferred) |
| 0x21 / 0x31 | GET / POST_MOTOR_LIMITS | | read limits |
| 0x22 / 0x32 | GET / POST_MOTOR_DIRECTION | | read direction |
| 0x1F / 0x2F / 0x3F | SET / GET / POST_FACTORY_DEFAULT | →motor | **reset positions** |
| 0x40 / 0x60 | GET / POST_NODE_ADDR | | discovery |
| 0x45 / 0x55 / 0x65 | GET / SET / POST_NODE_LABEL | | motor label |
| 0x4C / 0x6C | GET / POST_NODE_SERIAL_NUMBER | | serial |
| 0x50 | SET_NODE_DISCOVERY | | enter/exit discovery mode |
| 0x6F | NACK | motor→ | command rejected |
| 0x7F | ACK | motor→ | command accepted |

### 5.4 Payload formats

**CTRL_MOVETO (0x03)** — function byte + LE value:

| fn | meaning | bytes |
|----|---------|-------|
| 0x00 | to **down limit** (close) | `{0x00,0x00,0x00,0x00}` |
| 0x01 | to **up limit** (open) | `{0x01,0x00,0x00,0x00}` |
| 0x02 | to **IP** index | `{0x02, ip, 0x00, 0x00}` |
| 0x04 | to **percentage** | `{0x04, pct, 0x00, 0x00}` (pct 0–100) |

> These four match the current code exactly (move-up = fn 0x01, move-down = fn 0x00,
> move-to = fn 0x04). Keep them.

**CTRL_MOVEOF (0x04)** — *move-by-steps*, function byte + LE parameter:

| fn | meaning |
|----|---------|
| 0x00 / 0x01 | next IP down / up |
| 0x02 / 0x03 | move N **pulses** down / up |
| 0x04 / 0x05 | move N **×10 ms** down / up |

Layout: `{fn, param_lo, param_hi, 0x00}`.

**SET_MOTOR_LIMITS (0x11)** — `{fn, direction, param_lo, param_hi}`:

| fn | meaning |
|----|---------|
| 0x01 | set limit **at current position** (the common "set top/bottom here" op) |
| 0x02 | set at absolute pulse count (param) |
| 0x04 / 0x05 | adjust by ×10 ms / by pulses (param) |

direction: `0x00` = down limit, `0x01` = up limit. **⚠ confirm via Set Pro.**

**SET_MOTOR_DIRECTION (0x12)** — `{dir}`: `0x00` standard, `0x01` reversed. **⚠ confirm.**

**SET_FACTORY_DEFAULT (0x1F)** — payload not in public sources. **⚠ capture from Set Pro**
(this is the "reset positions" control; verify scope = limits-only vs full factory wipe).

**POST_MOTOR_POSITION (0x0D)** response: `data[0..1]` = LE pulse position, `data[2]` = **percent
0–100**, `data[3]` = tilt %, `data[4]` = IP (`0xFF` = none). `pulses==0xFFFF` or `pct>100`
⇒ position unknown / fault. **Use `data[2]` directly** — this is the position-reporting fix
(commit `6477359`); do not recompute from pulses.

**POST_MOTOR_LIMITS (0x31)** response: per public docs `data[0..1]` reserved, `data[2..3]` =
LE limit. ⚠ The current code reads up=`data[0..1]`, down=`data[2..3]` from a single query — this
disagrees and is a likely source of the position trouble. **Verify on the wire** (§13).

### 5.5 Position semantics & the HA inversion (important)

- **Somfy:** `pct = 0` → **up limit (open)**, `pct = 100` → **down limit (closed)**.
- **Home Assistant cover:** `position = 0` → **closed**, `position = 100` → **open**;
  `is_closed` when fully down.
- Therefore: **`ha_position = 100 − somfy_pct`** and **`somfy_pct = 100 − ha_position`**.
- HA *open* → CTRL_MOVETO fn 0x01 (up limit); HA *close* → fn 0x00 (down limit).

Define this conversion in **one place** (the WS/HA boundary) and keep firmware state in native
Somfy percent to avoid double-inversion bugs.

---

## 6. Firmware design

### 6.1 Stack & layout (PlatformIO / Arduino, TinyC6)

Mirror `actron-sniffer`:

```
somfy-sdn/
  platformio.ini          # um_tinyc6 (Arduino) + [env:native] host tests
                          # lib_deps: Links2004/WebSockets, bblanchon/ArduinoJson
  src/
    main.cpp              # WiFi/provisioning, HTTP server, WS loop hook, bus task spawn
    sdn.{h,cpp}           # pure C++ framing: build/parse/checksum/invert (port of app_sdn)
    bus.{h,cpp}           # UART owner: send/receive, retry, listen-only gate, sniffer
    devices.{h,cpp}       # device table (per node addr), discovery, per-device state
    state.{h,cpp}         # snapshot model serialised to WS/HTTP
    ws_api.{h,cpp}        # WebSocket command/state schema + ack correlation
    http_api.{h,cpp}      # debug endpoints + error-log ring + OTA + provisioning forms
    errlog.{h,cpp}        # bounded ring buffer of wire/protocol errors
    wifi_prov.{h,cpp}     # NVS creds, STA connect, SoftAP captive portal, GPIO0 button
  test/                   # native host tests (sdn framing, checksum, device table)
  WIRING.md
  CLAUDE.md               # to be written when code lands
```

`sdn.{h,cpp}` must be **pure C++ (no Arduino deps)** so the framing/checksum logic is host-unit-
tested under `[env:native]`, exactly like Actron's `bridge`/`state`.

### 6.2 Bus manager (FreeRTOS task)

A single dedicated task owns UART1 (Actron lesson: never share the UART with the Arduino loop).
SDN is request/response, so this is simpler than Actron's byte pump — but the same priority/
yielding discipline applies during OTA (suspend the task, drain FIFOs, force listen-only).

**Modes:**
- `LISTEN` (default, safe) — never transmits. Parses every frame seen on the bus (ours or a
  third party's), updating the device table and feeding the sniffer/error log. This is how we
  observe **Set Pro** or an existing controller. **TX is firmware-gated** — boot default is
  LISTEN; nothing is sent until explicitly armed.
- `ACTIVE` — the task may transmit: services queued commands, polls positions, runs discovery.

**Command queue:** WS/HTTP handlers enqueue a `{target_addr, msg, payload}` job and (optionally)
wait on an ack correlated by a job id. The task serialises all bus access — no concurrent TX.
Retry up to `SDN_RETRY_COUNT` (3) with `SDN_RETRY_DELAY_MS` (200), as today.

**Polling cadence (per motor):** fast (~100 ms) while a motor is moving, slow (~30–60 s) when
idle. Use task notifications so an inbound command wakes the task immediately (carried from
`app_sdn.cpp`). With multiple motors, round-robin idle polls to keep bus load sane.

**Coexistence caution:** if another live controller (e.g. a Somfy keypad) shares the bus,
two masters polling will collide. ACTIVE mode should be opt-in per deployment, and the spec
recommends LISTEN-only on buses with an existing controller until that controller is removed
or we accept the collision risk. Surface bus-busy/collision as a wire error.

### 6.3 Device table & discovery (multi-motor)

Per-device record:

```c
struct sdn_device {
  uint8_t  addr[3];          // raw (byte-reversed) node address
  char     label[17];        // from GET_NODE_LABEL, optional
  uint8_t  position_pct;     // native Somfy %, 0=open .. 100=closed
  uint16_t position_pulses;
  uint16_t up_limit_pulses, down_limit_pulses;
  uint8_t  direction;        // 0 std / 1 reversed
  movement_state movement;   // IDLE / MOVING_UP / MOVING_DOWN
  bool     fault;            // position unknown / stalled
  uint8_t  stall_count;
  uint32_t last_seen_ms;     // for comms-loss detection
  bool     online;           // last_seen within timeout
};
```

**Populating the table (three sources, in priority order):**
1. **Configured addresses** — known motor addresses (from Set Pro or the Somfy address-reader
   tool 9017142), provided at provisioning/config time. Most reliable on a populated bus.
2. **Active discovery** — `SET_NODE_DISCOVERY` + broadcast `GET_NODE_ADDR` (the sequence already
   in `sdn_discover_motor`). ⚠ With multiple motors, simultaneous responses **collide**; treat
   active discovery as best-effort for single-motor or one-at-a-time enrolment.
3. **Passive observation** — in LISTEN mode, any frame's source address registers/refreshes a
   device. Great for enumerating a bus non-invasively.

**Comms-loss:** a device goes `online=false` after `DEVICE_OFFLINE_MS` (e.g. 90 s) without a
valid frame; emit a state push and log it. Recovers automatically on the next valid frame.

### 6.4 Error & diagnostics model

A bounded **ring buffer** (`errlog`, e.g. last 128 entries) of structured events, browsable over
HTTP (`/errors`) and summarised in `/stats` counters:

| Class | Trigger |
|-------|---------|
| `CHECKSUM` | RX checksum mismatch |
| `FRAMING` | bad length / truncated / parity error |
| `TIMEOUT` | no/short reply within window |
| `NACK` | motor returned 0x6F |
| `STALL` | position unchanged during movement (fault) |
| `POS_UNKNOWN` | `pulses==0xFFFF` / `pct>100` |
| `OFFLINE` / `ONLINE` | comms-loss transitions |
| `COLLISION` | unexpected/garbled bus activity while transmitting |

Each entry: timestamp (ms since boot), device addr (or none), class, raw hex (truncated), short
message. Counters in `/stats` mirror Actron's `rx_err` style.

### 6.5 Concurrency model & OTA safety (carry the Actron learnings)

RS485 servicing is time-sensitive and **must not block the rest of the device** (WiFi, HTTP, WS,
OTA, mDNS). We adopt the same concurrency split that the Actron device arrived at — and, more
importantly, the same OTA teardown, because the Actron OTA *bricking* was caused precisely by
RS485 traffic backlogging during a flash. The mechanism (from `actron-sniffer/src/main.cpp`
`bridgeTask` / `handleUpdate`, and the "OTA hardening" note in its CLAUDE.md):

**Two-context split:**
- **Arduino main loop (low priority)** runs HTTP server, WebSocket (`webSocket.loop()`), mDNS,
  console, and **OTA (`httpUpdate`)**.
- **Dedicated FreeRTOS bus task (priority above the main loop)** exclusively owns UART1. Nothing
  else touches the UART. Commands are enqueued from the WS/HTTP handlers and serviced here, so a
  slow HTTP request can never stall bus timing, and bus activity can never stall HTTP/OTA.

**Where SDN legitimately differs from Actron — and the trap to avoid.** Actron's task is a
continuous byte-pump (a cut-bus MITM forwarding a live Modbus stream); it deliberately
*busy-loops* while bytes are pending so a burst is forwarded without preemption, yielding only
in the idle gap. SDN is **request/response**: our task should spend almost all its time in
**blocking, CPU-yielding waits** — `uart_read_bytes(timeout)` for replies and a
notification-wait (`ulTaskNotifyTake` with a timeout) between transactions (the model already in
`app_sdn.cpp`). That is inherently safer than Actron's busy-pump.

> **Do not** implement the RX path as a busy-poll (`while (Serial1.available()) …` without
> yielding, or a tight spin on a timeout). That is exactly the shape that starved Actron's main
> loop and stalled its OTA download. If any busy-wait is ever unavoidable, `vTaskDelay(1)` every
> iteration. Prefer blocking reads with timeouts throughout.

**Mandatory OTA teardown** (the brick fix — replicate exactly in the `/update` handler, *before*
calling `httpUpdate.update()`):
1. **Stop the bus task touching the UART** — set the mode flag to LISTEN *and* `vTaskSuspend()`
   the bus task. Flipping a flag alone is insufficient (Actron bug #2): if the task can still
   spin on a non-draining `available()`, it starves the priority-1 download. Suspend it outright;
   the device reboots on OTA success, so no resume is needed on the happy path.
2. **Drain the UART RX FIFO** — `while (Serial1.available()) Serial1.read();` so the transceiver
   isn't left holding a half-frame, and the FIFO can't keep waking anything mid-flash.
3. **Force LISTEN / no-TX** — guarantee no code path drives UART1 TX during the flash.
4. Only then `httpUpdate.update()`. On failure, restore mode + `vTaskResume()` + re-enable; on
   success it reboots from inside `httpUpdate`.

**Also carry:** the PlatformIO `__DATE__`/`__TIME__` content-hash cache gotcha — change file
*content* (not just `touch`) to refresh the `fw=` build string; verify it changed in `/stats`
after every reflash.

---

## 7. WiFi provisioning (replaces Matter commissioning)

**No baked-in credentials.** Credentials live in **NVS** (Arduino `Preferences`). Approach:
**SoftAP captive portal**, using the **GPIO0 momentary button** already wired on field devices.

**Boot flow:**
1. Read SSID/password from NVS.
2. If present → STA connect with timeout + N retries.
3. If absent, or connect fails repeatedly → start **SoftAP** `somfy-sdn-XXXX` (suffix from chip
   ID) and serve a **captive portal**: a form capturing SSID + password (and optionally the
   list of known motor addresses). Save to NVS, reboot into STA.

**Live re-config (no physical access):** an **authenticated** `POST /wifi` endpoint
(`{ssid, password}`) writes NVS and reboots. Auth = a device token / shared secret set at
provisioning (see security note). This is the normal "change the WiFi" path once a device is
in-wall and reachable on the current LAN.

**GPIO0 button (already wired):**
- **Long-press (~10 s):** wipe WiFi creds (and optionally device config) → reboot into SoftAP
  portal. This is the in-field recovery when the network changed and the device can't reconnect.
- **Short-press:** **WINK all detected devices** (CTRL_WINK 0x05 to every motor in the table) —
  a quick physical "which blinds is this controller driving?" confirmation during install.

**Live re-config:** an unauthenticated `POST /wifi` endpoint (`{ssid, password}`) writes NVS and
reboots — the no-touch path to change WiFi once the device is reachable on the current LAN.

**mDNS:** advertise `somfy-sdn-XXXX.local` once on STA, like Actron's hostname pinning.

**Security (initial rollout): none.** All HTTP/WS endpoints are open on the LAN, exactly like the
Actron device. The security boundary is the **restricted network** itself — the universal model
for every device in this estate — so per-device auth is deliberately a *later* refinement, not a
launch blocker. If/when we revisit, the obvious step is a per-device token on the mutating
endpoints (`/wifi`, TX-arm, `/send`, OTA); the spec leaves room for that but does not require it.

---

## 8. HTTP debug API

Operator / RE surface (LAN). All endpoints open (security = restricted network, §7).

| Endpoint | Purpose |
|----------|---------|
| `GET /` | help + status summary |
| `GET /stats` | one-line status: mode, devices online, wire-error counters, fw build, rssi, ip |
| `GET /devices` | JSON device table (addr, label, pct, limits, direction, fault, online, last_seen) |
| `GET /log?since=<seq>&n=<max>` | sniffed frames (incremental polling like Actron) |
| `GET /errors?n=<max>` | error ring buffer (newest first) |
| `POST /mode` | **TX gate.** `{set: listen\|active}`. Default LISTEN. |
| `POST /send` | build+send a raw SDN message, report reply — **the RE workhorse** (`{addr, msg, data}`) |
| `POST /discover` | run a discovery sweep |
| `POST /move` | convenience control (`{addr, cmd: open\|close\|stop\|pos, value}`) |
| `POST /update` | HTTP-pull OTA (`{url}`) |
| `POST /wifi` | set credentials (`{ssid, password}`), reboot |
| `POST /clear` | reset ring buffers + counters |

Read endpoints are `GET`; **mutating endpoints are `POST`** — proper REST, so they can't be
triggered accidentally by browser prefetch / crawlers / caching and their params don't leak into
GET-logged URLs. Params may be query-string or JSON body (`curl -X POST` handles both). This is a
deliberate departure from Actron's all-GET RE convention.

`/send` is the analogue of Actron's `/armwrite` + `/inject`: it lets us hand-craft any message
(e.g. a candidate `SET_FACTORY_DEFAULT`) and watch the reply — the primary tool for confirming
calibration payloads on the bench.

Frame log line format (carry Actron's): `<seq> <t_s> +<gap>us <len>: HEX… |ascii|`.

---

## 9. WebSocket Controller API (port 8767)

Runtime surface for HA. Push-based, same shape as Actron's WS API.

**Connect:** `ws://<host>:8767/`. On connect, the server sends a full `state` snapshot, then a
snapshot on every change plus a periodic heartbeat (~10 s). Availability in HA flips after ~30 s
of silence (coordinator pattern).

**Outgoing — `state`:**
```json
{ "type": "state", "data": {
    "mode": "listen|active",
    "devices": [
      { "addr": "01:00:23", "label": "Lounge",
        "position": 40,            // native Somfy % (0=open,100=closed); HA inverts
        "moving": "idle|up|down",
        "fault": false, "online": true,
        "up_limit": 1234, "down_limit": 9876, "direction": "std|reversed" }
    ],
    "errors_recent": 3
} }
```

**Outgoing — `ack` / `error`:** correlated by client-supplied `id`, as Actron
(`{"type":"ack","id":…,"status":"accepted"}` / `{"type":"error","id":…,"message":…}`).

**Incoming commands** (all carry `addr` to target a motor, optional `id`):

End-user (1:1 port):
- `open` — CTRL_MOVETO fn 0x01 (up limit)
- `close` — CTRL_MOVETO fn 0x00 (down limit)
- `stop` — CTRL_STOP
- `set_position` `{position}` — HA position; firmware inverts → CTRL_MOVETO fn 0x04

Calibration:
- `move_steps` `{direction, pulses}` — CTRL_MOVEOF fn 0x02/0x03
- `set_top_limit` — SET_MOTOR_LIMITS fn 0x01, dir up (at current position)
- `set_bottom_limit` — SET_MOTOR_LIMITS fn 0x01, dir down
- `set_direction` `{reversed}` — SET_MOTOR_DIRECTION
- `reset` — SET_FACTORY_DEFAULT (⚠ payload from Set Pro)
- `identify` — CTRL_WINK (0x05) — optional, handy for "which blind is this"

Admin:
- `set_mode` `{mode}` — listen/active (TX gate; same as HTTP, mirrored for HA)
- `rediscover`

Position writes have **no optimistic state** — firmware reports what the motor confirms via
polling, like Actron. Movement/limits transitions surface through the next `state` push.

---

## 10. OTA

Same HTTP-pull mechanism as Actron (the dev box is WSL → can't be the espota target). Host the
binary on atlas, device pulls via `/update`. Reuse the exact runbook from
`actron-sniffer/CLAUDE.md`. The **mandatory task-suspend / FIFO-drain / force-LISTEN teardown and
the build-string cache gotcha are specified in §6.5** — that teardown is what prevents the
RS485-backlog-during-OTA brick we hit on Actron; do not ship `/update` without it.

> Note for the rollout: the atlas OTA file server is ephemeral — see memory
> `actron-ota-server-ephemeral`. Start/verify it before each reflash.

---

## 11. Home Assistant component (`somfy_sdn`)

Lives at `home-assistant/custom_components/somfy_sdn/`. Combines the two existing patterns:
- **Connection/coordinator** = Actron MITM controller (config flow host+port, push WS, layered
  reconnect/availability logic). Reuse that code near-verbatim.
- **Entity** = DOSA cover semantics (`CoverEntity`, OPEN/CLOSE/STOP/SET_POSITION).

**Entities:** one `cover.somfy_<addr or label>` per discovered motor.
- `device_class = shade` (roller). `supported_features`: OPEN | CLOSE | STOP | SET_POSITION.
- `current_cover_position = 100 − somfy_pct`; `is_closed = somfy_pct == 100`;
  `is_opening/closing` from `moving`.
- Fault → entity attribute (and optionally a binary_sensor) so a stuck motor is visible.
- Comms-loss → entity `available = False` for that motor (per-device, not whole bridge).

**Calibration** — exposed as **services** (power-user, with target `entity_id`), since the cover
entity itself only does open/close/stop/position:
- `somfy_sdn.move_steps` `{direction, pulses}`
- `somfy_sdn.set_top_limit` / `somfy_sdn.set_bottom_limit`
- `somfy_sdn.set_direction` `{reversed}`
- `somfy_sdn.reset`
- `somfy_sdn.identify`
- `somfy_sdn.set_mode` `{mode}` (bridge-level)

Optionally surface common calibration ops as `button` entities for UI convenience.

**Diagnostics:** a `sensor`/`binary_sensor` per bridge for `mode`, `devices_online`,
`errors_recent`, plus per-device `online`/`fault`.

**Config flow:** host + port (default 8767), as Actron. Motor naming: like Actron zones, rename
via the HA UI (labels can also come from `GET_NODE_LABEL` if present).

---

## 12. Control-surface mapping (summary)

| Capability | Source | SDN message | Payload |
|------------|--------|-------------|---------|
| Open (to top) | 1:1 port | CTRL_MOVETO 0x03 | `{0x01,0,0,0}` |
| Close (to bottom) | 1:1 port | CTRL_MOVETO 0x03 | `{0x00,0,0,0}` |
| Stop | 1:1 port | CTRL_STOP 0x02 | `{0x01}` |
| Go to % | 1:1 port | CTRL_MOVETO 0x03 | `{0x04,pct,0,0}` (pct = 100−ha_pos) |
| Move by steps | **new** | CTRL_MOVEOF 0x04 | `{0x02|0x03, n_lo, n_hi, 0}` |
| Set top limit | **new** | SET_MOTOR_LIMITS 0x11 | `{0x01, 0x01, 0, 0}` ⚠ |
| Set bottom limit | **new** | SET_MOTOR_LIMITS 0x11 | `{0x01, 0x00, 0, 0}` ⚠ |
| Set direction | **new** | SET_MOTOR_DIRECTION 0x12 | `{0x00|0x01}` ⚠ |
| Reset positions | **new** | SET_FACTORY_DEFAULT 0x1F | ⚠ capture from Set Pro |
| Read position | port | GET_MOTOR_POSITION 0x0C → 0x0D | use `data[2]` % |
| Read limits | port | GET_MOTOR_LIMITS 0x21 → 0x31 | ⚠ verify byte offsets |
| Identify | optional | CTRL_WINK 0x05 | — |

---

## 13. Open items / reverse-engineering tasks

These are the ~10% not nailed by public sources. **Primary method: Somfy Set Pro in-app log** —
tell Set Pro to send the command to a (fake) device and read the exact TX bytes. Secondary:
capture on the wire with this device's own LISTEN/sniffer mode + `/send` to replay candidates.

1. **SET_MOTOR_LIMITS payload** — confirm the `{fn, direction, param}` layout and the
   direction enum (which value = up vs down limit) for "set limit at current position".
2. **SET_MOTOR_DIRECTION payload** — confirm `0x00`/`0x01` = standard/reversed (vs some other
   encoding) and whether a re-calibration is forced after a direction change.
3. **SET_FACTORY_DEFAULT (reset) payload** — not in public sources at all. Capture from Set Pro;
   confirm scope (limits-only reset vs full factory wipe incl. address/label).
4. **POST_MOTOR_LIMITS byte offsets** — public docs say `data[2..3]` = limit with `data[0..1]`
   reserved; the current code reads up=`[0..1]`, down=`[2..3]` from one query. Resolve on the
   wire — this likely explains some position-reporting trouble and should be fixed in the port.
5. **Multi-motor discovery collisions** — determine how Set Pro enumerates a populated bus (it
   likely relies on known addresses / one-at-a-time). Decide our enrolment UX accordingly.
6. **Coexistence** — confirm behaviour when an existing controller is on the bus (the motor
   currently misbehaving may be a coexistence/limits issue, not a comms one — LISTEN-mode
   capture should reveal which).

---

## 14. Build / test / deploy

- **Build/flash:** PlatformIO, `pio run -e um_tinyc6` + USB or OTA (`um_tinyc6_ota` env from a
  non-WSL LAN host, like Actron).
- **Host tests:** `pio test -e native` — cover SDN framing (build/parse roundtrip, checksum,
  inversion, boundary lengths), device-table updates, and the HA position inversion. Mirror
  Actron's host-test discipline (pure-C++ `sdn.{h,cpp}`).
- **HA deploy:** add `somfy_sdn` to the `./setup ha` flow; `--restart` for the new component.
- Each new top-level component gets a `CLAUDE.md` (per repo convention) once code lands; update
  the root `CLAUDE.md` directory table and `home-assistant/CLAUDE.md` component table.

---

## 15. References

- Existing implementation (the proven baseline): `matter-apps/common/features/app_sdn.{h,cpp}`
  and `matter-apps/common/CLAUDE.md` → "SDN Blinds Details".
- Design twin: `actron-sniffer/` (HTTP+WS+OTA patterns, FreeRTOS task discipline, OTA runbook).
- ccutrer/somfy_sdn — protocol reference & message IDs:
  <https://github.com/ccutrer/somfy_sdn/blob/master/doc/protocol.md>
- Cyberax/py-somfy-sdn — payload layouts (CTRL_MOVETO/MOVEOF, SET_MOTOR_LIMITS, direction):
  <https://github.com/Cyberax/py-somfy-sdn>
- Mark Baysinger, "Somfy Wire (RS-485) Protocol" (older ILT2 byte-level examples):
  <https://blog.baysinger.org/2016/03/somfy-protocol.html>
- Somfy SDN Integration Guide / SDN Motor Configuration Software programming guide (official,
  via service.somfy.com) — for message DATA structures.
- Somfy "Set Pro" software — **authoritative** byte source via its in-app send log (§13).
- Somfy RS485 Motor Limit Setting & Address Reader Tool (ref 9017142) — reads motor addresses
  for the configured-address enrolment path.
```
