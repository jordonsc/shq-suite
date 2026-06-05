# Actron NEO — Local Control Recipes

**Authoritative reference for the surgical-INJECT path against the in-wall MITM bridge.**

The bridge listens for the NEO's func-03 responses to the indoor board, rewrites specific
register bytes in flight, re-stamps Modbus CRC-16 on each modified frame, and forwards to the
board. The board treats the rewritten response as authoritative for its committed state.

All recipes assume:
- Hardware: TinyC6 + 3.3 V auto-direction RS485 transceivers in a cut-bus tap (see `WIRING.md`)
- Firmware: `fw="May 31 2026 00:54:45"` or newer, byte-forwarding pump on its dedicated
  FreeRTOS task (`bridgeTask`)
- Bridge in `inject` mode (`POST /bridge?mode=inject`)
- Rules set via `POST /inject?rules=<reg>:<val>,<reg>:<val>` (persistent) or
  `POST /pulse?rules=<reg>:<val>,...&n=<N>` (transient, expires after N recognised B frames)

**Restore step is implicit at the end of every recipe**: `POST /bridge?mode=passthru` to clear
all live rewrites. Inject rules persist across mode changes but only fire while mode is
`inject`.

**Commit latency**: the board accepts a pulse within one polling cycle (~3 s) but its
authoritative broadcast can lag 20–30 s before publishing the committed state. Wait at least
5 polling cycles (~15 s) before declaring a commit failed. The bridge's `B.mod` counter is a
more reliable "rule landed on a frame" signal than the broadcast.

---

## Register quick reference (page-1 = regs 2–125, page-2 = regs 126–247; values big-endian)

| Reg | Field | Notes |
|----:|-------|-------|
| **10** | Mode word | low nibble = mode (0=off, 1=cool, 2=heat, 3=fan, 4=auto); bit 6=turbo, bit 7=quiet, bit 9=away, bit 15=always set |
| **11** | Fan speed + cont-fan | low nibble = speed (1=low, 2=med, 3=high, 4=auto); bit 7 = continuous fan; high byte mostly 0x59 |
| **12** | Active main setpoint | BE 0.1 °C; mirrors active mode's store (reg 55 cool / 56 heat) |
| **14** | Command code | Pulse on user action; idle 0. Codes: 0x01 mode, 0x02 fan, 0x04 main setpoint, 0x40 zone enable. **Zone setpoints have NO pulse.** |
| **55 / 56** | Cool / heat main setpoint stores | per-mode shadows of reg 12 |
| **85** | Zone enable bitmask | bit N = zone N enabled. 0=Living, 1=Entry, 2=Bedroom, 3=Gym+Guest, 4=Sal, 5=Jordon, 6/7=unused |
| **94–101** | Live zone temps | NEO-relayed BLE sensor readings, 0.1 °C, per zone |
| **126** | Status / commit-window byte | low nibble carries active mode (1=cool, 2=heat); **load-bearing for zone-setpoint commits, see §4** |
| **127–134** | **Zone COOL setpoints** | per-zone, BE 0.1 °C, indexed 0=Living … 7=Z8 |
| **135–142** | **Zone HEAT setpoints** | per-zone, BE 0.1 °C |
| **143–150** | Per-zone block (unknown — see §5 auto-mode TBD) | values ~0x01B0..0x0220 |
| **151–158** | Per-zone bit-14 flags | `0x001E` = bit clear, `0x401E` = bit set; tracks which zones the controller has recent activity for |

Zone index → name: 0=Living, 1=Entry, 2=Bedroom, 3=Gym+Guest, 4=Room B, 5=Room A,
6=Z7 (unused), 7=Z8 (unused).

---

## §1 — Master mode change (off / cool / heat / fan / auto)

**Recipe** (transient pulse):
```
POST /bridge?mode=inject
POST /pulse?rules=10:<word>,14:0x01&n=2
```

**Build `<word>`** for reg 10:
- Start from current reg 10 in the NEO's report (preserves quiet/turbo/away flags)
- Replace low nibble (bits 0–2) with target mode: `0` off, `1` cool, `2` heat, `3` fan, `4` auto
- Keep bit 15 (`0x8000`) set
- Example: cool + quiet + always-set = `0x8081`. Heat + quiet = `0x8082`. Off + quiet = `0x8080`. Auto + quiet = `0x8084`.

**Proven this session**: heat → cool → off all committed via this recipe.

---

## §2 — Master fan speed change

**Recipe**:
```
POST /pulse?rules=11:<word>,14:0x02&n=2
```

**Build `<word>`** for reg 11:
- Start from current reg 11 (preserves continuous-fan bit + 0x59 high byte)
- Replace low nibble with target speed: `1` low, `2` med, `3` high, `4` auto
- Bit 7 (`0x80`) = continuous fan
- Example: med + cont-fan + 0x59 high = `0x5982`. Auto fan, no cont = `0x5904`.

Proven via 0x67 tap (FINDINGS §9); structurally identical to mode change so the MITM INJECT
recipe is high-confidence even though it wasn't directly retested this session.

---

## §3 — Master setpoint change

**Recipe**:
```
POST /pulse?rules=12:<val>,<active_store>:<val>,14:0x04&n=2
```

**Values**:
- `<val>` = setpoint in 0.1 °C, big-endian. E.g. 23.0 °C = `0x00E6`.
- `<active_store>` = the active mode's store: `55` if cool, `56` if heat. Both `<val>`s the
  same. Required because the board may snap reg 12 back from the store on the next mode
  toggle if the store wasn't updated.

Proven via 0x67 tap (FINDINGS §9). MITM INJECT identical mechanism.

---

## §4 — Zone setpoint change (THE NEW ONE)

This is the recipe that took the most digging — see §9 of FINDINGS for the timeline.

**Recipe** (persistent inject, NOT pulse):
```
POST /bridge?mode=inject
POST /inject?rules=<reg>:<val>,126:<mode_bits>
… wait 10–60 s …
POST /bridge?mode=passthru
```

**Values**:
- `<reg>` = the zone setpoint register:
  - Cool: 127=Living, 128=Entry, 129=Bedroom, 130=Gym+Guest, 131=Sal, 132=Jordon, 133=Z7, 134=Z8
  - Heat: 135=Living, 136=Entry, 137=Bedroom, 138=Gym+Guest, 139=Sal, 140=Jordon, 141=Z7, 142=Z8
- `<val>` = new setpoint in 0.1 °C, big-endian. E.g. 22.0 °C = `0x00DC`, 21.5 °C = `0x00D7`.
- **`<mode_bits>`** for reg 126 = **which array you're writing**, not the master mode (see §6):
  - Cool-array write (regs 127–134) → `0x0001`
  - Heat-array write (regs 135–142) → `0x0002`
  - **Without this register set, the board IGNORES the setpoint change.** This is the
    side-channel signal — the NEO sets reg 126.lo low nibble to the array bit when it's
    actively reporting a value change; in steady state both bits are clear. Synthetic INJECT
    must reproduce this or the board won't re-read the setpoint registers. Confirmed in cool,
    heat, AND auto master modes (auto inherits the per-array bit, not its own).

**No reg 14 pulse** — zone setpoints are value-only.

**Multiple zones at once**: yes, list multiple setpoint regs in the same rules string. The
single reg 126 = `0x000X` rule covers all of them since they all gate on the same byte.

**Proven this session**:
- `/inject?rules=130:0x00D2,126:0x0001` — Gym COOL → 21.0 °C ✓
- Earlier RESPOND tests with the modified-saved Living-HEAT capture worked because the saved
  payload's reg 126 had the right low-nibble value (heat = 0x62 lo byte → bit 1 set).
  Synthetic INJECT without the reg 126 rewrite **always failed**.

---

## §5 — Zone enable mask change ("zone reshuffling")

**Recipe** (pulse):
```
POST /pulse?rules=85:<mask>,14:0x40&n=2
```

**Values**:
- `<mask>` = bit-N for zone N. `0x0001` = Living only, `0x0008` = Gym only, `0x002A` =
  Entry+Gym+Jordon, etc.
- `14:0x40` is the zone-enable command pulse (mapped this session; same bitfield pattern as
  the other commands).

Fires on both enable AND disable (reg 85 mask change in either direction). The board
publishes the new committed mask in its next page-1 broadcast.

**Proven this session**: enable Entry (`0x0001 → 0x0003`), disable Living (`0x0003 →
0x0002`), and zones → Gym only (`0x0020 → 0x0008`) all committed.

---

## §6 — Auto-mode zone setpoints

**Resolved 2026-05-31.** Auto mode does **not** have its own setpoint register array. Each
zone has separate cool and heat setpoints regardless of master mode, stored in the same
two arrays as the dedicated cool/heat modes:

- Cool array: regs 127–134
- Heat array: regs 135–142

When the master mode is auto, the NEO displays both setpoints per zone and the user can
press either. The reg 126 low nibble **always tracks which array is being touched**, not the
master mode:

| Array being written | reg 126.lo low nibble |
|---|---|
| Cool array (127–134) | `0x01` |
| Heat array (135–142) | `0x02` |

This was confirmed by capturing a Gym setpoint press while in auto mode: reg 130 (Gym COOL)
changed and reg 126 = `0x0001` (not `0x0004` = auto-mode bit). Both array recipes have now
been validated directly via the `/inject` path:

- Cool: `/inject?rules=130:0x00DC,126:0x0001` → Gym COOL committed (multiple values)
- Heat: `/inject?rules=138:0x00EB,126:0x0002` → Gym HEAT committed (24.0 → 23.5 in auto mode)

**Implication for the §4 recipe**: nothing changes. The `<mode_bits>` field in reg 126.lo
is `0x01` for cool-array writes and `0x02` for heat-array writes, **regardless of what
master mode the system is currently in**. Treat it as a "which array I'm writing" signal,
not a "current master mode" signal.

The per-zone block at regs 143–150 (values `0x01B0..0x0220`) is unrelated — it didn't change
when Gym setpoint was pressed. Still unmapped (likely board telemetry).

---

## Other open items

- **Away / turbo / continuous-fan toggles**: command codes still to be mapped via
  `tools/findpulse.py`. Bits 3 / 4 / 5 / 7 of the reg 14 bitfield are the unallocated
  candidates (0x01 mode, 0x02 fan, 0x04 main setpoint, 0x40 zone enable are taken).
- The `xx_store` per-mode shadow registers for fan / mode haven't been needed in our writes
  so far; the existing mode-pulse + reg 10 rewrite is enough to set the active state.

---

## HA integration shape

The bridge exposes everything over HTTP on `http://REDACTED-IP/` (pinned LAN IP) /
`redacted.local`. A HA custom component can issue these inject recipes from its
service handlers and reconcile against the board's broadcast (read via `GET /log` or
`GET /stats`). Cloud `nimbus` API is demoted to telemetry / fallback.

When implementing the HA component, **always `/disarm` or set `/bridge?mode=passthru` before
issuing a new command** to avoid stale rules interfering. The bridge's `B.mod` counter
incrementing is a useful "rule landed" signal between issue and broadcast publication.
