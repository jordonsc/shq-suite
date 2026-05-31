# NEO page-1 + page-2 responses — Entry zone enabled

**Captured:** post-FreeRTOS-task firmware, fw="May 30 2026 23:36:12"
**Device state at capture:** bridge=passthru, B.mod=0, A.mod=0 — bytes forwarded unchanged.
**Trigger:** user toggled Entry zone ON on the NEO touchscreen.

State transition (reg 85, zone enable mask):
- Before: `0x0001` — Living only (bit 0)
- After:  `0x0003` — Living + Entry (bits 0+1) ✓ confirms FINDINGS §7 bit-N=zone-N convention

## NEW FINDING — reg 14 command code 0x40 = zone enable

**Reg 14 (command-type code) pulsed `0x0000 → 0x0040 → 0x0000` for exactly one response cycle
on the zone-enable press, mirroring the pattern documented for mode/fan/main-setpoint:**

| Command | reg 14 pulse | value register(s) |
|---------|:---:|---|
| HVAC mode | 0x01 | reg 10 low nibble |
| Fan speed | 0x02 | reg 11 low byte |
| Main setpoint | 0x04 | reg 12 + active-mode store (55/56) |
| **Zone enable** | **0x40** | **reg 85 (bit-N=zone-N)** ← NEW |

This is a clean bitfield (bits 0/1/2/6), so codes 0x08, 0x10, 0x20, 0x80 are plausible
candidates for on/off, away, turbo, continuous-fan — to be mapped via the same
clear→change→`findpulse.py` loop. **Update FINDINGS.md §9 command-vocabulary table** when
verifying these next.

Issuable from 0x67 emulator (page-1 territory) — the same pulse mechanism that works for
mode/fan/setpoint should work for zone enable.

## Sequence (reg 14 and reg 85 across the cycle)

| Frame | t (s) | reg 14 | reg 85 |
|-------|------:|:------:|:------:|
| B 3512 | 1430.432 | 0x0000 | 0x0001 |
| B 3520 | 1433.433 | **0x0040** ← pulse | **0x0003** ← new mask |
| B 3528 | 1436.434 | 0x0000 (reverted) | 0x0003 (persisted) |

## NEO page-1 response (regs 2–125)

Frame: `B 3520 t=1433.433000s +2242000us 253 bytes`
First B-side page-1 carrying the zone-enable command pulse + the new mask.

```
66 03 F8 00 6E 26 23 03 2B 01 18 96 49 18 04 05 F3 00 00 80 82 59 04 00 EB 00 D8 00 40 00 00 75 30
00 D8 01 F6 00 18 00 00 00 00 1D 06 34 BF 20 02 00 3E 03 9D 00 44 00 29 75 30 75 30 02 ED 00 29 00
13 01 5B 0B B9 03 00 0F EF 75 30 75 30 00 2C 00 3E 00 54 04 7E 05 46 05 DC 00 0A 00 00 01 75 03 DE
00 00 CC EF 00 14 00 93 02 19 00 E6 00 EB 01 04 00 C8 00 B4 01 18 FB 02 FB 02 00 0F 00 1E D9 32 00
00 C8 23 02 26 00 32 01 2C 00 16 00 D8 75 30 75 30 01 00 01 00 01 00 01 00 01 00 01 00 00 00 00 3F
00 3F 00 00 00 03 29 09 29 09 29 09 29 09 29 09 29 09 00 01 00 01 00 D8 00 C4 00 DB 00 D2 00 C6 00
F5 00 D8 00 D8 00 64 00 00 00 00 00 00 00 00 00 00 00 64 00 64 00 01 00 07 00 08 00 09 00 0A 00 0B
00 01 00 01 64 00 64 00 64 00 64 00 64 00 64 00 64 00 64 00 AD BD
```

- Header: `66 03 F8` (slave 0x66, func 03, bytecount 0xF8 = 248)
- Data: 248 bytes covering registers 2..125 inclusive
- CRC trailer: `AD BD` (Modbus CRC-16, little-endian)

Key register positions in this frame (data offset relative to byte 3 of frame):

| Reg | Offset | Bytes | Value | Meaning |
|----:|-------:|:------|:------|:--------|
| 10  | 16–17 | `80 82` | 0x8082 | mode word: heat (nibble 2), quiet (bit 7), always-set (bit 15) |
| 11  | 18–19 | `59 04` | 0x5904 | fan auto (low nibble 4) |
| 12  | 20–21 | `00 EB` | 235 | active main setpoint 23.5 °C |
| 14  | 24–25 | `00 40` | **0x0040** | **command pulse: zone-enable** |
| 22  | 40–41 | `1D 06` | freshness counter |
| 28  | 52–53 | `00 29` | freshness counter |
| 35  | 66–67 | `0B B9` | freshness counter |
| 37  | 70–71 | `0F EF` | freshness counter |
| 55  | 106–107 | `00 E6` | main cool store 23.0 |
| 56  | 108–109 | `00 EB` | main heat store 23.5 |
| 85  | 166–167 | `00 03` | **zone enable mask: Living + Entry** |
| 94–101 | 184–199 | `00 D8 00 C4 00 DB 00 D2 00 C6 00 F5 00 D8 00 D8` | live zone temps (NEO-relayed) |

## NEO page-2 response (regs 126–247)

Frame: `B 3522 t=1433.935000s +241001us 249 bytes`
Page-2 from the same polling cycle.

```
66 03 F4 00 20 00 E6 00 E6 00 D2 00 D2 00 E1 00 D2 00 D7 00 D7 00 DC 00 DC 00 E6 00 F0 00 F0 00 F0
00 E6 00 E6 01 DA 02 0B 02 0F 02 09 02 18 02 0D 02 06 02 00 00 1E 40 1E 40 1E 40 1E 40 1E 40 1E 00
00 00 00 00 C4 00 DB 00 D2 00 C6 00 F5 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30
75 30 01 00 01 00 01 00 01 00 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 01 DA 75 30 75 30 02 3C 02 18 01 F8 02 54 01 D6 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30
75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75
30 75 30 75 30 00 02 00 02 00 02 00 02 00 02 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 07 29 28 03 20 00 3C FF 3E 00 24 59 CF
```

- Header: `66 03 F4` (slave 0x66, func 03, bytecount 0xF4 = 244)
- Data: 244 bytes covering registers 126..247 inclusive
- CRC trailer: `59 CF` (Modbus CRC-16, little-endian)

Page-2 holds zone setpoints (regs 127–142) and various sensor / derived fields — none of
these changed for a zone-enable press, so the page-2 here is essentially the steady-state
snapshot during the press cycle (useful as a reference baseline alongside the page-1 pulse).

## Board's confirming page-1 broadcast

Frame: `A 3525 t=1435.419000s +494003us 253 bytes`
First indoor-board broadcast of the new state after the press. Reg 85 = `0x0003` matches the
NEO's reported mask → board accepted and committed. Reg 14 in the broadcast stays at `0x0000`
(the command pulse is only on the controller side; the board's broadcast doesn't carry it).

```
00 10 00 04 00 7A F4 03 2B 01 18 96 49 18 04 05 F3 00 00 80 82 59 04 00 EB 00 CE 00 00 00 00 75 30
00 CE 01 F5 00 18 00 00 00 00 20 06 34 BF 20 02 00 3E 03 9D 00 44 00 29 75 30 75 30 02 EB 00 28 00
13 01 5B 0B B9 02 FF 0F AA 75 30 75 30 00 2C 00 3E 00 54 04 7E 05 46 05 DC 00 0A 00 00 01 75 03 DE
00 00 CC EF 00 14 00 93 02 19 00 E6 00 EB 01 04 00 C8 00 B4 01 18 FB 02 FB 02 00 0F 00 1E D9 32 00
00 C8 23 02 26 00 32 01 2C 00 16 00 D8 75 30 75 30 01 00 01 00 01 00 01 00 01 00 01 00 00 00 00 3F
00 3F 00 00 00 03 29 09 29 09 29 09 29 09 29 09 29 09 00 01 00 01 00 D8 00 C4 00 DB 00 D2 00 C6 00
F5 00 D8 00 D8 00 64 00 0B 00 00 00 00 00 00 00 00 00 64 00 64 00 01 00 07 00 08 00 09 00 0A 00 0B
00 01 00 01 64 00 64 00 64 00 64 00 64 00 64 00 64 00 64 00 98 0F
```

- Header: `00 10 00 04 00 7A F4` (write-multiple-regs broadcast to addr 0x00, start reg 4, qty 0x7A=122, bytecount 0xF4=244)
- Reg 85 = data offset (85-4)*2 = 162..163 = `00 03` ✓ matches NEO

## Notes for replay templates

`bridge::StreamingBridge` expects payloads **without** the 3-byte slave/func/bytecount header
but **with** the 2-byte CRC trailer:

- `kReplayPage1Bytes = 250` → bytes 3..252 of the page-1 frame above (data + CRC)
- `kReplayPage2Bytes = 246` → bytes 3..248 of the page-2 frame above (data + CRC)

Captured CRCs (`AD BD` for page-1, `59 CF` for page-2) are valid for this exact payload.

## Implication for HA integration

To enable a zone from the 0x67 emulator: issue
`/armwrite?addr=0x67&ovr=85:<new_mask>&pulse=14:0x40&pulsen=1`
(mirroring the proven mode/fan/setpoint pattern with the new command code). Untested but
the architecture is identical — worth a quick verification next session.
