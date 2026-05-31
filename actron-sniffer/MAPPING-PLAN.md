# Actron NEO RS485 — Full Mapping Plan

> **STATUS (2026-05-31): this plan is FULLY EXECUTED — see [`FINDINGS.md`](FINDINGS.md)
> §7/§9/§10 and [`LOCAL-CONTROL-RECIPES.md`](LOCAL-CONTROL-RECIPES.md) for the current state.**
> Phases 0–4 (clean capture, checksum, register map) done. Phase 5 (write/control) done via
> both the 0x67 tap command-pulse path and the MITM-bridge `/pulse` + `/inject` path.
> **Zone-setpoint INJECT mechanism fully solved 2026-05-31**: the side-channel signal is
> reg 126.lo low nibble = active-mode bit (cool=1, heat=2). Remaining mapping work: auto-mode
> zone setpoints (likely regs 143–150, TBD) and the **away / turbo / continuous-fan** command
> codes. The text below is the original plan, kept for historical context; trust FINDINGS and
> LOCAL-CONTROL-RECIPES where they differ.

---

**The objective is reliable local *control* (writes).** Cloud writes are the thing that's
broken — they 200-OK and silently don't apply. This whole exercise exists to send commands the
A/C actually obeys, **especially per-zone setpoints** (the one thing the ICUNO-MOD Modbus path
can't do).

Reading/mapping is the **prerequisite, not the goal**: you cannot forge a command the system
accepts without knowing the exact frame layout **and the checksum**. A bad-CRC frame is
ignored. So the passive capture is our disassembler — every diff below exists to let us
construct (and later inject) a *valid* write.

Method for the mapping steps: **change one variable at a time on the NEO touchscreen, capture
before/after, stable-diff, decode.** Stay receive-only **until** Phase 5, where we deliberately
start transmitting.

> **Fast track to a proven write:** you don't have to finish the entire map first. The minimum
> to attempt control is: decode the **zone-setpoint frame** (mostly done — @36/@32) + the
> **checksum** (Phase 1.3) + the write hardware (Phase 5). Prove one write, then expand. Treat
> the full map (Phases 2–4) as breadth you can fill in once control is demonstrated.

---

## Phase 0 — Make the capture clean (do this first)

The 128-byte `FRAME_MAX` split is the main friction. Before serious mapping:

1. **Reflash the firmware** (requires one USB connection — bring the TinyC6 to the PC briefly):
   - `FRAME_MAX` → **512** (and drop `RING` to ~128 to stay within RAM). Whole messages, no
     splitting, no reassembly guesswork.
   - Add **ArduinoOTA** (`ArduinoOTA.begin()` in `setup()`, `ArduinoOTA.handle()` in `loop()`)
     so every future reflash is over WiFi — no more wall access.
   - Sanity-check RAM after: ring at 128×~526 B ≈ 67 KB; bump the `formatFrame` line buffers
     (currently `char line[700]`) to ~2300 to fit a 512 B frame's hex.
2. **Reusable analysis tool — DONE:** `tools/decode.py` (`snapshot` = live register map from the
   func-0x10 broadcasts, `regs FILE`, `diff BEFORE AFTER`, `crc`). It's the workhorse below;
   extend it as fields are mapped.
3. Re-confirm baud `9600/8N1`, `rx_err≈0`.

## Phase 1 — Frame anatomy

For each message type (`00 10 00 04`, `00 10 00 7e`, `66 03 f8 00`, `66 03 f4 00`, the 8-byte
`xx 03 00` ones):

1. Capture many instances of each at steady state. Identify which **bytes are constant**,
   which **drift slowly** (live temps/sensors), and which look like **counters/timestamps**.
2. Decode the **header**: confirm whether byte 3 (`04/7e/f8/f4`) is a subtype/page and whether
   any byte is a **length** or **sequence number**.
3. **Checksum — SOLVED.** It's standard **Modbus CRC-16 (poly 0xA001, init 0xFFFF, low byte
   first on the wire)**, validated on full frames. No RE needed — just compute it.
4. **Roles — largely SOLVED (it's Modbus master/slave).** Indoor board = **master** (sends
   func-03 reads + func-0x10 broadcast writes to addr 0x00); wall controllers = **slaves**
   (NEO=0x66; 0x67/0x68 empty slots). Still worth a final confirm the master is the indoor
   board — capture a button-press: a pure slave puts nothing on the wire until polled.

## Phase 2 — Per-zone mapping (the core deliverable)

Establish the **zone array layout and index→zone mapping**.

1. **Setpoints, one zone at a time.** For each of the 6 zones, set a **unique, distinctive**
   target (stagger them, e.g. Living 21.0, Entry 21.5, Bedroom 22.0, Gym+Guest 24.5,
   Room B 25.0, Jordon 23.5 — all within ±2 °C of main). Capture after each. The offset
   that changes per zone = that zone's setpoint slot. Result: a table of `zone → offset`.
   - We already know **one slot = @36 in `00 10 00 7e`** (the zone changed during research,
     Room A). Confirm the array stride and the other five.
2. **Live temperatures.** Correlate the live-temp array (~@74+ in `00 10 00 7e`) against the
   touchscreen's per-zone room temps; map array index → zone. (Hard to force, so just read
   and match to displayed values.)
3. **Zone enable/disable.** Toggle each zone on/off individually; find the **bitmask** byte(s)
   and confirm **bit ↔ zone**. (Early hint: a byte `0x23↔0x33` and a `0x00↔0x02` — re-derive
   with clean capture.) Note the system quirk: turning off the last active zone shuts the
   whole system down.

## Phase 3 — System-wide controls

One variable at a time; capture before/after; record message + offset + encoding:

- **Main/master setpoint** (we have main *temp* at @24; find the editable main **setpoint**).
- **System on/off.**
- **HVAC mode**: cool / heat / auto / fan-only — find the mode field + value mapping.
- **Fan speed**: low / med / high / auto.
- **Continuous fan**, **away**, **quiet**, **turbo** toggles.
- **Compressor / running state**, **damper position** per zone (likely the `0x64→0x28`
  i.e. 100→40 values seen — possibly damper %).
- Cross-check the **±2 °C-from-main clamp** and **0.5 °C step** behaviour in the encoding
  (does crossing main change anything? earlier "flag" hypothesis to settle).

## Phase 4 — Consolidate

- Produce a **field map table**: `message type | offset | size | endianness | encoding |
  meaning | zone index | notes`. Add it to `FINDINGS.md`.
- Checksum is **Modbus CRC-16** (poly 0xA001, init 0xFFFF, appended low-byte-first) — known.
- Record the register map as **Modbus register numbers** (reg = frame start + data_offset/2).
- Note any fields still unexplained (counters, sentinels, the `66 03`/`00 10` direction).

## Phase 5 — WRITE: emulate a controller (the actual point)

Plan A is to **be a Modbus slave at an empty controller slot (0x67/0x68)** and answer the reads
the board already sends — reporting our desired setpoints (see `FINDINGS.md` §9 for the full
emulator state machine). Hardware: just move **DE+/RE from GND to a GPIO** so we can transmit.

**5.1 — The make-or-break test (do before building anything).** With TX enabled, answer a
single `67 03 …` poll with a valid response = (latest broadcast) + **one distinct setpoint**.
Watch: does the broadcast — and the real NEO's screen — **adopt** it?
- **Adopts →** controller-emulation works. Build Plan A.
- **Ignores (board only honours commissioned controllers) →** go to 5.2.

Risks to respect: respond within the Modbus turnaround (~few ms); never collide with 0x66
(don't answer polls addressed to the real NEO); watch for controller-identity registers that
must be set for 0x67, not copied from 0x66.

**5.2 — Fallback: transparent MITM bridge.** Cut the bus; ESP between NEO and indoor with
**two transceivers**, rewriting target fields in-flight. Add a **normally-closed failsafe
relay** hard-bridging NEO↔indoor on power loss. Timing-critical; recompute CRC on modified
frames.

**5.3 — Build it.** Implement the emulator state machine + HA integration so commands go
**local-first** (bus), with the cloud demoted to fallback/telemetry. Prototype **one** field
with the A/C watched closely before generalising.

## Working discipline

- **One variable per step.** Batched changes (as happened in research) make attribution hard.
- After each change, note the **exact** on-screen value (including 0.5° rounding) before
  diffing.
- Keep captures: save each labelled capture (`<state>.log`) so diffs are reproducible.
- Stay **receive-only** until Phase 5 is deliberately entered.
