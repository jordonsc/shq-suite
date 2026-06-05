# Actron NEO RS485 — Research Findings

Reverse-engineering the proprietary RS485 bus between the **NEO wall controller (NTW-1000)**
and the **indoor unit** (the "IDU Interface"), to get reliable **local** monitoring and
eventually **control** of the Actron A/C — escaping the unreliable `nimbus` cloud.

> **Status (snapshot):** Bus reading is **solved**; the register map is decoded and value-verified
> (§7); values are **big-endian** Modbus. Hardware: auto-direction transceiver + OTA +
> `FRAME_MAX=512`; TX is firmware-gated (no wiring change needed to write).
>
> **✅ LOCAL WRITE CONTROL — FULLY PROVEN via TWO independent paths.**
>
> Path A (tap mode, FINDINGS §9): emulate a secondary controller at **0x67**, answer its page-1
> poll, fire a **command pulse** — the board adopts & commits. Non-invasive, NEO stays live at
> 0x66. Page-1 commands only.
>
> Path B (MITM bridge inject, **2026-05-31**): cut the bus, rewrite the real NEO's 0x66 page-1
> and/or page-2 response in flight using the bridge's `/inject` + `/pulse` endpoints. Same
> command-pulse mechanism works — the board doesn't care which slot the pulse comes from.
> **All commands proven via this path:** mode change, zone enable mask, fan/setpoint by
> analogy, and **zone setpoints (cool array confirmed; heat array predicted)** via a new
> side-channel finding (§9): the board requires **reg 126.lo low nibble** to carry the active
> mode's low bit (cool=1, heat=2) before it re-reads zone setpoint registers. Without that
> byte set, persistent INJECT of the setpoint alone is silently ignored. With it set, the
> board commits within ~3 s.
>
> Path C (MITM **RESPOND** — block NEO + replay a saved response, **2026-05-31**): a stricter
> variant of Path B. `/bridge?mode=respond` drops both directions of forwarding entirely (NEO
> is isolated), and the firmware emits a saved page-1/page-2 byte-for-byte (data + original
> CRC) in reply to the board's 0x66 polls. Templates loaded via `/loadtemplate?page=1|2&hex=…`.
> Direct confirmation that **the board accepts any CRC-valid 0x66 response as authoritative for
> value-only registers** — replaying a captured response with `reg 135 = 0x00DC` got the board
> to broadcast 22.0 °C within ~3 s, while reg 10 / 12 / 85 (pulse-gated commands) in the same
> payload were correctly ignored because the saved capture had `reg 14 = 0` (no pulse). On
> exit, the NEO resynced to the board's broadcast on its next poll cycle.
>
> The control model: **the board owns authoritative state and ignores merely-*reported* values;
> it acts on command *pulses* (reg 14).** Command codes mapped: **mode = 0x01, fan = 0x02,
> main setpoint = 0x04, zone enable = 0x40.** Zone setpoints are value-only (no pulse).
>
> **Bridge timing was the load-bearing fix** (§9): earlier MITM attempts failed because the
> bridge pump shared the Arduino main loop with HTTP/WiFi/OTA and inserted **>t3.5 mid-frame
> gaps** on the destination wire when stalled — board rejected frames on CRC. Fix: pump on a
> dedicated FreeRTOS task at priority 5 — see `src/main.cpp` `bridgeTask`. Both PASSTHRU and
> INJECT now byte-clean.
>
> **Commit-to-broadcast latency is significant.** The board accepts a pulse immediately, but its
> own broadcast can take **20–30 s** to publish the new state (mode-off observed); shorter
> changes (zone enable) ~3–6 s. HA reconcile logic must tolerate this — don't declare a command
> failed for at least 5+ broadcast cycles (~15 s).
>
> **Open items:** (a) command codes for **away / turbo / continuous-fan** still to map (page-1,
> quick via `findpulse.py`). (b) build the emulator + HA integration — local-first writes via
> MITM inject (covers everything) OR 0x67 tap (page-1 only); cloud demoted to telemetry.
> A new agent can continue from "§10 Next steps" using the live device + `tools/decode.py` +
> `tools/findpulse.py`.

---

## 1. Why we're doing this

- The `actron_shq` HA integration is cloud-only (`nimbus.actronair.com.au`). The cloud
  frequently **accepts commands (HTTP 200) but never applies them** — observed first-hand:
  a `set_hvac_mode` retry loop re-sent ~18 times over 90 s and still didn't land.
- The official **ICUNO-MOD Modbus** path can do whole-house control but **cannot do per-zone
  setpoints** (see [`../docs/actron-local-control.md`](../docs/actron-local-control.md)).
- The **NEO wall controller is the true coordinator**: it holds per-zone setpoints, reads the
  wireless BLE zone sensors, and drives the indoor unit over this RS485 bus. So this bus is
  where the per-zone data lives — confirmed.

## 2. System under test

- Wall controller **NTW-1000** (NEO Touch), outdoor **CRV240T**, indoor fw 3.43.
- **6 zones**: Living, Entry, Bedroom, Gym + Guest Rooms, Room B, Room A.
- Wireless **BLE** zone sensors (NSB-10/NSW-10).
- **Constraints (from the user, important for decoding):**
  - A zone's target can only be **±2 °C from the main temperature** (main is variable; the board
    enforces the clamp and rewrites out-of-range zone setpoints itself).
  - The touchscreen adjusts in **0.5 °C increments**.

## 3. Hardware as currently deployed

A **parallel tap** on the NEO↔indoor RJ45 bus, installed in the wall:

- **Unexpected Maker TinyC6** (ESP32-C6) + a **3.3 V auto-direction TTL↔RS485 module** (no DE/RE
  pin — it keys the driver off UART TX). `RO→GPIO17 (RX)`, `DI→GPIO16 (TX)`.
- Tap on the RJ45 (T568B): **485 A = blue / pin 4**, **485 B = white-blue / pin 5**,
  **GND ref = white-brown / pin 7**. +12 V (brown/orange) is **not** connected to the bridge.
- **Read/write capable.** It's a 2-wire shared bus, so our TX reaches both the NEO and the board.
  Receive-only is enforced **in firmware** (never write to the bus) except during a deliberate
  controller-emulation write; default state is disarmed/receive-only.
- Pass-through wiring so the **NEO stays fully live**; TinyC6 powered from a USB charger.
- Full wiring in [`WIRING.md`](WIRING.md).

**Network:** on WiFi `SHQ`, **pinned IP `REDACTED-IP`** (also `redacted.local`). It is
**not** on USB anymore — all interaction is over HTTP.

## 4. Firmware & tooling

PlatformIO project in this directory (`platformio.ini`, `src/main.cpp`), pioarduino platform,
board `um_tinyc6`. See [`CLAUDE.md`](CLAUDE.md). **PlatformIO is installed at `~/.pio-venv`.**

```bash
~/.pio-venv/bin/pio run -d actron-sniffer                         # build
~/.pio-venv/bin/pio run -d actron-sniffer -t upload --upload-port /dev/ttyACM0   # flash (needs USB)
```

**HTTP API** (poll from any LAN host; the dev box reaches it fine):

| Endpoint | Use |
|----------|-----|
| `GET /log?since=<seq>&n=<max>` | captured frames; `since` = incremental (header carries `seq_max`) |
| `GET /stats` | status (baud, parity, gap, counts, `ip`) |
| `GET /set?baud=&parity=N\|E\|O&gap=<us>` | change capture settings live (**use `gap=5000`** — see §9 capture tip) |
| `GET /measure` | pulse-width baud estimate (needs active bus) |
| `GET /clear` | reset ring + counters |
| `GET /armwrite?addr=&ovr=reg:val,…&pulse=reg:val&pulsen=N&turn=us` | **arm controller emulation** (write). addr 0x66/0x67/0x68; `ovr`=persistent register overrides; `pulse`=one-shot command edge for N page-1 responses; `turn`=reply turnaround µs (≥5000). e.g. setpoint→22.0 from 0x67: `addr=0x67&ovr=12:220,56:220&pulse=14:4` |
| `GET /disarm` | back to receive-only |
| `GET /txprobe` | TX self-test (inject a poll to 0x66, report if it answers) |
| `GET /update?url=<bin>` | HTTP-pull OTA |

Frame line format: `<seq> <t_s> +<gap>us <len>: HEX...  |ascii|`

**Analysis recipe that works:** `tools/decode.py` (the workhorse) pulls `/log`, decodes the
func-0x10 broadcasts into a register map, and **stable-diffs** before/after a known change —
registers that move are the deliberate change (live temps jitter; known counters are filtered
by eye). Workflow: `decode.py snapshot --save before.log` → make **one** change on the NEO →
`snapshot --save after.log` → `diff before.log after.log`. Decode values as **16-bit
big-endian (standard Modbus), ÷10 = °C** for temperatures.

## 5. Bus parameters — SOLVED

- **9600 baud, 8N1.** Confirmed by `rx_err = 0` over tens of thousands of bytes. Wrong
  baud/parity would flood framing errors. (The `/measure` pulse estimator is unreliable here
  because the bus is bursty/idle; we didn't need it.)
- **Protocol = Modbus RTU.** Confirmed: standard **CRC-16 (poly 0xA001)** validates on full
  frames; func **0x03** (read holding registers) and func **0x10** (write multiple registers)
  framing matches exactly. 9600/8N1 is the Modbus default — that's why it worked first try.
  This **supersedes the earlier "proprietary, not Modbus" assumption** (see
  `../docs/actron-local-control.md`). The checksum problem is therefore **solved** — it's
  standard Modbus CRC-16.

## 6. Protocol structure — what we know

- **Bursty.** A burst of several messages fires within a few hundred ms, then the bus goes
  idle for ~0.2–5 s before the next burst. (So `/measure` often sees 0 edges — it's just idle.)
- **Message framing:** the firmware groups bytes into a frame, breaking on an idle gap.
  Genuinely separate messages are **6 ms–500 ms** apart. `FRAME_MAX` is now **512** so whole
  messages are captured (no more 128 B splitting). **Set the gap to 5 ms** (`/set?gap=5000`): a
  long response can have a >3 ms *mid-message* pause that splits the frame and drops a
  single-cycle command pulse — at gap=3000 we lost ~half the pulses (see §9). Genuine
  inter-message gaps are ≥6 ms, so 5 ms is safe.
- **Endianness:** register values are **big-endian** (standard Modbus — high byte first on the
  wire). Temperatures are **0.1 °C, absolute** (e.g. `00 EB` = 0x00EB = 235 = 23.5 °C). Not
  relative to main. ⚠️ A long-running **little-endian assumption was wrong** — it byte-swapped
  every value and is the root of the historical "endianness confusion". (The CRC trailer is the
  one little-endian exception: appended low byte first, per Modbus.)
- **`30 75` recurs** (= 0x3075 = 12405) in the big broadcast — a **"unused / not-present"
  sentinel** filling unmapped register slots.
- **Three broadcasts** seen: regs **4–125** (`00 10 00 04`), **126–248** (`00 10 00 7e`), and an
  extended page at **5000–5085** (0x1388+, where the volatile counters live).

### Modbus message map (CONFIRMED)

A single **master** (the indoor board — inferred, but forced: Modbus is single-master, so if
multiple NEOs can share the bus the controllers must be slaves) polls **slave controllers** and
broadcasts state. Wall controllers are slaves at **0x66 / 0x67 / 0x68**; only **0x66 (the NEO)**
is populated — **0x67 and 0x68 are empty controller slots** (polled, no response).

| On the wire | Modbus meaning |
|-------------|----------------|
| `66 03 00 02 00 7C` (8B) | read regs **2–125** from 0x66 → response `66 03 F8 …` (248 B = 124 regs) |
| `66 03 00 7E 00 7A` (8B) | read regs **126–247** from 0x66 → response `66 03 F4 …` (244 B) |
| `67 03 …`, `68 03 …` (8B) | reads to controllers 0x67 / 0x68 — **no response** (empty slots) |
| `00 10 00 04 00 7A …` | **func 0x10 write-multiple to addr 0x00 (broadcast)**, regs **4–125** |
| `00 10 00 7E 00 7B …` | broadcast write, regs **126–248** |

The board polls each controller, then **broadcasts its own authoritative state** (write to 0x00)
for all controllers to display. ⚠️ The broadcast is **not** a mere echo of the controller's
report — the **board owns the state** and ignores merely-reported values (it acts on command
pulses, §9). In steady state a controller's report ≈ the broadcast *except* the per-report
counters (regs 22/28/35/37, which advance) — so 0x66's response is still a good **register
template** for forging our own emulated responses. Byte offsets in §7 map to **Modbus register
numbers** (register ≈ start + data_offset/2).

## 7. Decoded fields

Fields are indexed by **absolute Modbus register number** (from the func-0x10 broadcast
headers). **All values big-endian.** Every field below was **confirmed by an isolated
single-variable change + `diff`** unless noted. Zone index = same order in every per-zone array
(0 = Living … 7 = Zone 8).

**Zone index → name** (used by all per-zone arrays):

| idx | zone | idx | zone |
|----|------|----|------|
| 0 | Living | 4 | Room B |
| 1 | Entry | 5 | Room A |
| 2 | Bedroom | 6 | Zone 7 (unused) |
| 3 | Gym + Guest | 7 | Zone 8 (unused) |

**CONFIRMED register map:**

| Register(s) | Field | Encoding / values |
|-------------|-------|-------------------|
| **127–134** (cool) / **135–142** (heat) | **Zone setpoints** ×8, **per-mode arrays** | BE, 0.1 °C, one per zone index. The active array tracks the mode. **page-2** — writing these is the unsolved item (§9). |
| **10** (0x0A) | **Mode / status word** (packed, 16-bit) | bits 0–2 = mode: **0=off, 1=cool, 2=heat, 3=fan, 4=auto**. **bit6 (0x40)=turbo, bit7 (0x80)=quiet, bit9 (0x0200)=away.** bit5 (0x20) = board-managed standby/idle status. bit15 (0x8000) always set. |
| **11** (0x0B) | **Fan speed + continuous-fan** | low nibble = speed **1=low, 2=med, 3=high, 4=auto**; **bit7 (0x80) = continuous fan**. High byte 0x59 = other status. |
| **12** (0x0C) | **Active main setpoint** | BE 0.1 °C — mirrors the active mode's store (reg 55/56, or 57/58 when away) |
| **13** (0x0D) | **Main / return-air current temp** | BE 0.1 °C — the indoor unit's aggregate "system temp" (what the NEO shows as current). Tracks a blend of the *enabled* zone sensors (≈ avg; confirmed 19.1 °C for enabled {18.4, 16.4, 22.5} on 2026-06-05; = the lone Living zone 21.5 °C when Living-only). **Mirrored at reg 17.** Decoded into `ControllerState.main_temp_raw`; published as the master's `current_temp`. |
| **55 / 56 / 57 / 58** (0x37–0x3A) | **Main setpoint stores: cool / heat / away-cool / away-heat** | BE 0.1 °C — per-mode shadows; reg 12 snaps to the relevant one on a mode/away change |
| **85** (0x55) | **Zone-enable bitmask** | bit N = zone N enabled (e.g. 0x33 = Living+Entry+Sal+Jordon on) |
| **94–101** (0x5E–0x65) | **Live zone current temps** ×8 — **NEO-forwarded sensor data** | BE 0.1 °C (drifts). idx 0=Living…; confirmed reg 94 = 21.3 °C matched the NEO's displayed Living temp exactly (2026-05-30). The NEO is the sensor conduit (zone sensors pair to it, not the board), so these are *relayed* readings — the board has no other source. 143–150 (~47–52) is likely the per-zone humidity the same sensors report (unconfirmed). |

**Write implications (important):**
- To set the **main setpoint** robustly, set reg 12 *and* the active mode's store (cool→55,
  heat→56, away→57/58), or the board may snap it back on the next mode toggle. *Confirmed
  bidirectionally* in both cool and heat (only the active mode's store moves; the others hold).
- The **±2 °C-from-main clamp is enforced by the indoor board**: when main moved, the board
  rewrote out-of-range zone setpoints itself (e.g. Jordon 19.5 → 20.0). A forged zone write
  outside the window will be overridden — respect the clamp.
- **System on/off** is not a separate bit: **off = reg 10 mode-nibble 0**, **on = set the
  desired mode (1–4)** — confirmed both ways. The feature bits (quiet/turbo/away) **persist
  across off**. No remembered-mode register appears in the broadcast (NEO restores its last
  mode from internal state — irrelevant to bus writes, we always command an explicit mode).

**SUSPECT / secondary (not control inputs — board-computed status):**
- reg **33** (0x21): per-zone **demand/damper-open** mask (gained bit 2 when Bedroom enabled;
  jumps with mode). Distinct from the enable mask (reg 85).
- reg **104/105** and **102/103/106/107** (0x66–0x6B): per-zone **demand / damper %** (0↔100,
  track thermal state).
- reg **196**, and the **5000-page** (0x1388+, esp. 5037/5056/5064/5065): free-running
  counters / live compressor+coil telemetry — **ignore in diffs**.

**Control surface is now fully mapped.** Only loose end: the register holding the NEO's
*remembered* mode while powered off (so on→restores last mode) — not needed to drive on/off
(off = reg 10 mode-nibble 0).

> **Scope note (user decision):** away / turbo / continuous-fan **are in scope** (mapped above).
> The only thing out of scope is the **quiet *schedule*** — the NEO runs quiet on a timer, and
> that schedule state is likely part of the persistent 5000-page churn; we don't read or manage
> it (quiet *state* itself = reg 10 bit 7, fine to expose). Target write surface = **zone
> setpoints, mode, main setpoint, fan speed, zone enable, on/off, away/turbo/continuous-fan.**

## 8. Gotchas / lessons (read before continuing)

- `FRAME_MAX` is now **512** — whole frames, no more 128-byte split/reassembly. (The old <2 ms
  rejoin rule is moot but `decode.py`'s `rejoin()` keeps it harmlessly.) OTA also works now
  (HTTP-pull via atlas — see `CLAUDE.md`), so all iteration is wireless.
- Values are **big-endian** and **absolute 0.1 °C**. The earlier *little-endian* claim was the
  mistake — it byte-swaps everything (e.g. a real `0x00EB`=235 looked like `0xEB00`=60160).
- **HA/cloud can be stale** — when the NEO is off the bus, `actron_controller_state` goes
  `timeout` and HA values freeze. The **bus is truth**; use HA only as a loose cross-check.
- There are **multiple sub-messages sharing the `00 10` header** — disambiguate by the full
  4-byte header **and length**, not just the first 2 bytes.
- The transceiver is an **auto-direction TTL↔RS485 module** (no DE/RE pin — it keys the driver
  off UART TX). So "receive-only" is enforced **in firmware** (the sniffer never writes to the
  bus); transmitting just means writing to the UART. No hardware change is needed to enable
  writes — only firmware. Stay receive-only until the deliberate 0x67-reply test.

## 9. Path to control — emulate a controller (Modbus slave)

**The goal is writes.** The Modbus model gives a clean route that *isn't* MITM: **act as a
secondary wall controller — a Modbus slave at an empty slot (0x67 or 0x68)** — and answer the
reads the board already sends every cycle, reporting our desired setpoints. The board merges
multi-controller input and broadcasts the result; the real NEO syncs to it. The auto-direction
transceiver can already transmit (it keys off UART TX) — **enabling writes is a firmware change,
not a wiring change**; CRC is standard Modbus.

**Premise CONFIRMED:** the board actively polls the empty slots — captured **`67 03 00 02 00 7C`
and `68 03 …` ~every 3 s**, identical to the 0x66 read, with **no response** (so the slot is
free). So there is a real, regular poll waiting for us to answer — we don't have to inject
unsolicited; we just reply in the existing turnaround window. The poll reads **regs 2–125** (qty
0x7C); a valid reply is `67 03 F8 <248 B regs 2–125> <CRC-lo CRC-hi>`. **Answered:** 0x67/0x68 are
**page-1-only** even when responding — only 0x66 is ever polled for page 2 (regs 126–247). That's
why zone setpoints (page-2) can't be set from a 0x67 slot.

**Emulator state machine** (mirrors our HA optimistic-overlay reconcile, one layer down):
1. **Boot:** don't answer 0x67 polls yet — no register template. Wait for a broadcast.
2. **On broadcast:** adopt it as our cache (full register set); expose to HA over the API.
3. **API client sets a field:** store it as an **overlay** on the cache.
4. **On later broadcast:** update all fields *except* ones with a pending overlay.
5. **When polled at 0x67:** respond with cache + overlays applied (valid Modbus response + CRC).
6. **Reconcile:** when a broadcast shows our overlaid value **was adopted** (`broadcast ==
   desired`), **clear that overlay** (so later NEO-side changes still propagate); time out if
   never adopted. *Don't* blindly clear after one transmit — that risks premature revert or a
   flip-flop war with the NEO.

> The pre-test hypotheses below (identity register, merge/conflict rule) were **answered** by the
> Write-test results that follow: there's no identity gate (parroting 0x66's payload + the
> right address byte is accepted), and the board doesn't merge *reported values* at all — it acts
> on **command pulses**. Kept for context; the results section is authoritative.

**~~Watch~~ (resolved):** no controller-identity register matters — a `66 03 F8` report and the
broadcast differ only in counters (22/28/35/37), and our 0x67 emulation is accepted with 0x66's
parroted payload. The Modbus address byte is the only identity, and we set it correctly.

**~~Make-or-break unknown~~ (resolved below):** the board doesn't honour *reported* values from
any slot — it acts on **command pulses**, which it honours even from an un-commissioned 0x67.

### Write-test results (2026-05-30) — ✅ LOCAL WRITE CONTROL ACHIEVED (non-invasive)

Firmware: `/armwrite?addr=&ovr=reg:val,...&pulse=reg:val&pulsen=N&turn=us`, `/disarm`, `/txprobe`.

**The model (this is the key insight):** the indoor board holds **authoritative state** and
generates the broadcast from it (incl. its own live sensor temps). A wall controller does **not**
set state by *reporting values* — the board ignores reported values. It sets state by sending a
**command pulse**. That's why every static-value test (at 0x66, 0x67, 0x68) was ignored.

**How a setpoint change works on the wire** (captured from a real NEO +0.5 ° press): the
controller writes the new value to **reg 12 (active) + reg 56 (heat store)** *and* **pulses
reg 14 = 4 for one response cycle** (idle 0 → 4 → 0). reg 24 also steps 0x2000→0x2002 (a board-set
"setpoint changed" flag — the board does this itself; we don't need to). The `reg 14 = 4` pulse is
the command edge; the value rides in reg 12/56.

**Proven (non-invasive, NEO stays live):** emulate a **secondary controller at 0x67** — answer its
page-1 poll (`67 03 00 02 00 7C` → `67 03 F8 …`) with the new value in reg 12/56 **and a one-shot
reg 14 = 4 pulse**. Result: the board **adopted and committed** the setpoint (24.0 → 22.0), the
broadcast updated, and it **persisted after we disarmed** — while the real NEO kept answering
0x66 normally and followed the new value. So:
- Static value-reports from a secondary slot are ignored; **command pulses are honoured.** (This
  overturns the earlier "board ignores un-commissioned slots" conclusion — it ignores their
  *values*, not their *commands*.)
- **0x67/0x68 are page-1-only secondary-controller slots** (board never polls them page 2 — only
  0x66 gets both pages). The setpoint command lives entirely in page 1.
- No commissioning, no MITM, no unplugging needed.

Modbus **t3.5 @ 9600 ≈ 3.65 ms**; reply with ≥ 5 ms turnaround (firmware default, tunable).

### Command vocabulary (mapped 2026-05-30)

**reg 14 (0x0E) is a command-type code**: idle 0, pulses a code for **one** response cycle on a
user action; the new value rides in the relevant register(s). Mapped via `tools/findpulse.py`
(clear → one NEO change → decode the 0x66 *responses*, find the pulse). Codes confirmed
**mode-independent** (setpoint pulse = 4 in both heat and cool):

| Command | reg 14 pulse | value register(s) | proven path(s) |
|---------|:---:|-------------------|:---|
| HVAC mode | **0x01** (bit 0) | reg 10 low nibble (0=off, 1=cool, 2=heat, 3=fan, 4=auto) | ✅ 0x67 tap; ✅ MITM 0x66 inject (2026-05-31 — heat→cool→off all committed) |
| Fan speed | **0x02** (bit 1) | reg 11 low byte (1=low, 2=med, 3=high, 4=auto) | ✅ 0x67 tap |
| Main setpoint | **0x04** (bit 2) | reg 12 (active) **+** active-mode store (reg 55 cool / 56 heat) | ✅ 0x67 tap |
| **Zone enable** | **0x40** (bit 6) | reg 85 (bitmask, bit-N = zone-N) | ✅ MITM 0x66 inject (2026-05-31 — Living+Entry → Jordon-only committed); fires on enable AND disable |
| **Zone setpoint** | **none — value-only**, but requires **reg 126.lo low nibble** = mode bit (cool=`0x01`, heat=`0x02`) as a side-channel commit signal | page-2 array: regs 127–134 (cool) / **135–142 (heat)** | ✅ MITM 0x66 inject (2026-05-31 — Gym COOL 21.0 → 22.0 → 22.5 → 21.0 committed via persistent `/inject?rules=130:val,126:0x0001`). Real-NEO press also works (NEO naturally sets reg 126 low nibble when reporting a change). |

- Codes 0x01 / 0x02 / 0x04 / 0x40 are a **bitfield** (bits 0/1/2/6). Bits 3/4/5/7 are
  candidates for **away, turbo, continuous-fan** — to be mapped via the same
  `clear → change → findpulse.py` loop. reg 24 also steps on each command (board-set
  command counter/flag — we don't send it).

### Zone-setpoint side-channel — **reg 126.lo low nibble** (2026-05-31)

The page-2 status byte at **reg 126** carries which **setpoint array** (cool vs heat) the
controller is actively writing, in its **low nibble**. Captured patterns:

- Living-HEAT change (master mode = heat): reg 126 = `0x0062` → low nibble `0x2` (heat-array)
- Entry-HEAT change (master mode = heat):  reg 126 = `0x0002` → low nibble `0x2`
- Gym-COOL change   (master mode = cool):  reg 126 = `0x0021` → low nibble `0x1` (cool-array)
- Gym-COOL change   (master mode = **auto**): reg 126 = `0x0001` → low nibble `0x1` (cool-array — confirms it's array-indexed, not master-mode-indexed)
- Random idle (any mode): reg 126 = `0x0000`, `0x0020`, `0x0060` — **low nibble `0x0`**

The board uses this nibble as a "controller has a real change to commit on this array"
indicator. The recipe is `0x01` for cool-array writes (regs 127–134) and `0x02` for
heat-array writes (regs 135–142), regardless of master mode. Auto mode uses both arrays
(both setpoints are visible per zone). Inject recipe (see `LOCAL-CONTROL-RECIPES.md` §4):

```
GET /inject?rules=<setpoint_reg>:<value>,126:0x000X    # X = 1 cool-array, 2 heat-array
```

Both array recipes proven via direct INJECT this session — Gym COOL across multiple values,
Gym HEAT 24.0 → 23.5. Without the reg 126 component, the same rule left for 90+ seconds
was ignored.
- **Per-mode arrays exist for zone setpoints too:** like the main cool/heat stores (reg 55/56),
  zones have a **cool array (127–134)** and a **heat array (135–142)**, indexed 0=Living … 7=Z8.
- **✅ Zone setpoints — SOLVED via MITM bridge (2026-05-30 evening).** No special command
  pulse exists; the NEO's *own* page-2 response (reg 127–134 cool / 135–142 heat) IS the commit
  signal. The board reads the value from the NEO's response, accepts it, and broadcasts back.
  **The earlier "0x66 impersonation rejected" + "real NEO press snaps back through the bridge"
  symptoms were a single root cause: bridge pump latency.** The pump shared the Arduino main
  loop with HTTP / WiFi / OTA / mDNS work; any stall of >~3 ms during a 253-byte forwarded
  response inserted a >t3.5 idle gap on the destination wire (the peer's TX FIFO drains to
  empty in ~1 ms at 9600 baud, so even short stalls leak through). The board treats >t3.5 as
  premature frame-end → drops the NEO's response on CRC. Visible in the capture as split
  frames with 6 ms+ inter-byte gaps (e.g. `B 27` 16-byte fragment + `B 28` 237-byte fragment).
  - **Fix:** `bridgeTask()` in `src/main.cpp` — pumpCapture() on a dedicated FreeRTOS task at
    priority 5 (above the Arduino loopTask at 1, below WiFi/lwIP at 18+). Busy-loops while the
    bus is active, vTaskDelay(1)s only when both UARTs are idle. After the fix: zero split
    frames over a 30 s window; physical NEO presses commit and persist (board + NEO agree at
    the new value within one broadcast cycle).
  - **Implication for impersonation:** the prior "0x66 impersonation rejected" result is
    actually about the same latency bug — the impersonation responses were sent from the
    Arduino loop in tap mode too. Worth re-testing the freshness-counter theory cleanly now,
    but for the deployed use-case it's moot: MITM passthru + inject covers zones, 0x67
    pulse covers system controls.
  - **Implication for INJECT mode:** the same fix applies — INJECT just adds register
    substitution + CRC re-stamp to PASSTHRU. With the pump task pinned, INJECT timing should
    now be solid too.
- **Capture tip:** the pulse is a single frame — set **`gap=5000`** (`/set?gap=5000`) so a
  >3 ms mid-response pause doesn't split the frame and drop the pulse (genuine inter-message gaps
  are ≥6 ms). At gap=3000 we missed ~half the pulses to split frames.

**Still to map:** away / turbo / continuous-fan. Then build the emulator + HA integration
(local-first writes via MITM inject — covers all commands and is now the recommended path; 0x67
tap is a fallback for page-1 commands when the bus isn't physically cut).

### ⚠️ Commit-to-broadcast latency (2026-05-31) — **load-bearing for test methodology**

**A command pulse is accepted by the board within one polling cycle, but the board's own
broadcast can take 20–30 seconds (sometimes longer) to publish the resulting state change.**
Observed during MITM inject tests:
- Mode change heat → cool: board broadcast updated ~100 s after the pulse (slowest)
- Mode change cool → off: ~30 s latency; board broadcast went `cool` → `cool+standby (bit 5)` → `off`
- Zone enable mask change: ~6 s (one or two broadcast cycles, fastest observed)

**Implication for testing**: a single rapid check after a pulse will MISS the change. Wait at
least 5 broadcast cycles (~15 s, ideally 30+ s) before declaring a command failed. This very
likely contaminated several of the "didn't work" observations from earlier this session — for
example, the initial "test 1 mode change didn't commit" reading was actually just impatience;
the same state did commit a minute later. Re-examine other "negative" results through this lens
before treating them as protocol findings.

**Implication for HA reconciliation**: don't retry / declare-failed based on a single
post-command broadcast. The MITM bridge can confirm the pulse was emitted (B.mod++ on a
recognised page-1) — that's a more reliable success signal than waiting for the broadcast to
catch up. Use the broadcast only for steady-state cross-check.

> Earlier pessimistic notes in git history (commissioning required / go MITM) are **superseded**
> by this result. **MITM inject is now the recommended primary path** — it covers all commands
> including zone setpoints, with the same pulse mechanism as 0x67 tap mode for page-1 things.

## 10. Next steps (summary)

1. ~~Reflash `FRAME_MAX ≥ 512` + OTA + transmit-capable hardware~~ — **DONE.** Whole-frame
   capture, HTTP-pull OTA, auto-direction transceiver (TX is firmware-gated).
2. ~~Map the register table~~ — **DONE** (§7): zone setpoints (per-mode), mode, fan, main setpoint
   (+per-mode stores), zone enable, live temps (NEO-forwarded), on/off, away/turbo/quiet/cont-fan
   bits.
3. ~~Make-or-break test~~ — **✅ DONE, SUCCESS** (§9): local write control via a 0x67 command pulse;
   board adopts, commits, broadcasts; NEO stays live.
4. ~~Map command vocabulary~~ — **mostly DONE** (§9 table): mode=0x01 (incl. on/off via nibble 0),
   fan=0x02, main setpoint=0x04, zone enable=0x40 — all mode-independent, all proven.
   **Remaining (quick, page-1, NEO live):** away / turbo / continuous-fan — same
   clear→change→`findpulse.py` loop. Codes likely fall on bits 3 / 4 / 5 / 7 (bitfield gap).
5. ~~Zone-setpoint write~~ — **✅ DONE** (§9): MITM bridge in PASSTHRU mode with the pump on a
   dedicated FreeRTOS task forwards NEO presses cleanly; the board accepts the value from the
   NEO's normal page-2 response. INJECT mode (register substitution + CRC re-stamp) shares the
   same forwarding path and works equivalently for HA-driven writes without a physical press.
6. ~~All page-1 commands via MITM inject~~ — **✅ DONE 2026-05-31**: mode change (heat→cool→off)
   and zone-enable mask change both verified via `/bridge?mode=inject` + `/pulse?rules=...`.
   Same command pulse codes as 0x67 tap; the board doesn't care which slot the pulse comes from.
   See `captures/` for the saved transition payloads.
7. **Build the emulator + HA integration:** firmware exposes HTTP endpoints that HA hits
   for each write. Recommended primary path: **MITM bridge inject** — `/bridge?mode=inject` +
   `/pulse` covers all command types (mode, fan, setpoint, zone enable, zone setpoints) from
   a single API. 0x67 tap mode is a fallback when the bus isn't physically cut. HA writes go
   **local-first** with cloud demoted to telemetry. Reconcile against the broadcast, but
   tolerate 20–30 s commit latency (see §9 latency note).
