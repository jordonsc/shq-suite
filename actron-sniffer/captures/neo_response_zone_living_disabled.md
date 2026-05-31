# NEO page-1 + page-2 responses — Living zone disabled (first-transition)

**Captured:** post-FreeRTOS-task firmware, fw="May 30 2026 23:36:12"
**Device state at capture:** bridge=passthru, B.mod=0, A.mod=0 — bytes forwarded unchanged.
**Trigger:** user toggled Living zone OFF on the NEO touchscreen.

**Provenance / transition proof:**
- B 5823 t=2298.187 → reg 14 = `00 00`, reg 85 = `00 03` (Living+Entry, OLD)
- **B 5831 t=2301.187 → reg 14 = `00 40`, reg 85 = `00 02` (Entry-only, NEW)** ← FIRST B page-1 carrying the change + pulse
- B 5839 t=2304.187 → reg 14 = `00 00`, reg 85 = `00 02` (pulse reverted, mask persisted)
- No intermediate B page-1 frame between 5823 and 5831.

## Confirmed: reg 14 = 0x40 fires on zone-enable mask **change**, not just enable

This is the second independent observation of reg 14 = 0x40. Both the **enable** test
(`neo_response_zone_entry_enabled.md`, 0x0001 → 0x0003) and this **disable** test
(0x0003 → 0x0002) produced the same one-cycle pulse. So the command code's semantics are
"zone enable mask changed", not "zone enabled". Same pattern as the other mapped codes —
the value rides in reg 85, and reg 14 just signals that it should be committed.

## NEO page-1 response (regs 2–125) — THE PRIMARY ARTEFACT

Frame: `B 5831 t=2301.187000s +2241000us 253 bytes`
First B-side page-1 carrying the zone-disable command pulse + the new mask.

```
66 03 F8 00 6E 26 23 03 2B 01 18 96 49 18 04 05 F3 00 00 80 82 59 04 00 EB 00 D0 00 40 00 00 75 30
00 D0 01 F4 00 18 00 00 00 0E 39 06 34 BF 20 02 00 5E 03 F7 00 43 00 1E 75 30 75 30 03 75 00 1B 00
19 01 5B 0D 53 02 C5 1A D9 75 30 75 30 00 2C 00 3E 00 54 04 7E 05 46 05 DC 00 0A 00 00 01 75 03 DE
00 00 CC EF 00 14 00 A5 02 7B 00 E6 00 EB 01 04 00 C8 00 B4 01 18 FB 02 FB 02 00 0F 00 1E D9 32 00
00 C8 23 02 26 00 32 01 2C 00 1E 00 DB 75 30 75 30 01 00 01 00 01 00 01 00 01 00 01 00 00 00 00 3F
00 3F 00 00 00 02 29 09 29 09 29 09 29 09 29 09 29 09 00 01 00 01 00 DB 00 C6 00 DC 00 D2 00 CB 00
F8 00 DB 00 DB 00 64 00 64 00 00 00 00 00 00 00 00 00 64 00 64 00 01 00 07 00 08 00 09 00 0A 00 0B
00 01 00 01 64 00 64 00 64 00 64 00 64 00 64 00 64 00 64 00 29 22
```

- Header: `66 03 F8` (slave 0x66, func 03, bytecount 0xF8 = 248)
- Data: 248 bytes covering registers 2..125 inclusive
- CRC trailer: `29 22` (Modbus CRC-16, little-endian)

Key positions:

| Reg | Data offset | Bytes | Value | Meaning |
|----:|------------:|:------|:------|:--------|
| 14  | 24–25 | `00 40` | **0x0040** | **command pulse: zone-enable-mask changed** |
| 85  | 166–167 | `00 02` | **0x0002** | **new mask: Entry-only (bit 1)** |
| 22  | 40–41 | `0E 39` | freshness counter |
| 28  | 52–53 | `00 1E` | freshness counter |

## NEO page-2 response (regs 126–247) — paired

Frame: `B 5833 t=2301.689000s +241001us 249 bytes`
Page-2 from the same polling cycle. Does NOT carry the change (zone-enable lives in page-1).

```
66 03 F4 00 08 00 E6 00 E6 00 D2 00 D2 00 E1 00 D2 00 D7 00 D7 00 DC 00 DC 00 E6 00 F0 00 F0 00 F0
00 E6 00 E6 01 D8 02 09 02 0D 02 08 02 16 02 0A 02 03 01 FE 00 1E 00 1E 40 1E 40 1E 40 1E 40 1E 00
00 00 00 00 C6 00 DC 00 D2 00 CC 00 F8 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30
75 30 01 00 01 00 01 00 01 00 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 01 D8 75 30 75 30 02 3B 02 16 01 F8 02 4E 01 D1 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30
75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75
30 75 30 75 30 00 02 00 02 00 02 00 02 00 02 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 07 29 28 03 20 00 3C FF 64 00 24 62 5B
```

- Header: `66 03 F4` (slave 0x66, func 03, bytecount 0xF4 = 244)
- Data: 244 bytes covering registers 126..247 inclusive
- CRC trailer: `62 5B` (Modbus CRC-16, little-endian)

## Board's confirming page-1 broadcast

Frame: `A 5836 t=2303.173000s +494002us 253 bytes`
First indoor-board broadcast after the commit. Reg 85 in the broadcast = `0x0002` ✓ matches.

```
00 10 00 04 00 7A F4 03 2B 01 18 96 49 18 04 05 F3 00 00 80 82 59 04 00 EB 00 C6 00 00 00 00 75 30
00 C6 01 F4 00 18 00 00 00 0F 00 06 34 BF 20 02 00 5E 03 F7 00 44 00 1E 75 30 75 30 03 75 00 1B 00
19 01 5B 0D 50 02 C5 1A DB 75 30 75 30 00 2C 00 3E 00 54 04 7E 05 46 05 DC 00 0A 00 00 01 75 03 DE
00 00 CC EF 00 14 00 A5 02 7B 00 E6 00 EB 01 04 00 C8 00 B4 01 18 FB 02 FB 02 00 0F 00 1E D9 32 00
00 C8 23 02 26 00 32 01 2C 00 1E 00 DB 75 30 75 30 01 00 01 00 01 00 01 00 01 00 01 00 00 00 00 3F
00 3F 00 00 00 02 29 09 29 09 29 09 29 09 29 09 29 09 00 01 00 01 00 DB 00 C6 00 DC 00 D2 00 CB 00
F8 00 DB 00 DB 00 59 00 64 00 00 00 00 00 00 00 00 00 64 00 64 00 01 00 07 00 08 00 09 00 0A 00 0B
00 01 00 01 64 00 64 00 64 00 64 00 64 00 64 00 64 00 64 00 F6 C3
```

- Header: `00 10 00 04 00 7A F4` (write-multiple-regs broadcast, start reg 4, qty 0x7A=122, bytecount 0xF4=244)
- Reg 85 at data offset (85-4)*2 = 162..163 = `00 02` ✓ matches NEO
- Reg 14 in the broadcast = `0x0000` (board's broadcast carries the *result*, not the command — the pulse is only on the controller-side report)

## Notes for replay templates

`bridge::StreamingBridge::setReplayPage1Data` (250 B) and `setReplayPage2Data` (246 B) take
the bytes from offset 3 onwards of the corresponding frame, **including** the trailing CRC.

Captured CRCs (`29 22` for page-1, `62 5B` for page-2) are valid for this exact payload.
