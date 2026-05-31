# NEO page-1 + page-2 responses — Entry HEAT @ 22.0 °C (first-transition)

**Captured:** post-FreeRTOS-task firmware, fw="May 30 2026 23:36:12"
**Device state at capture:** bridge=passthru, B.mod=0, A.mod=0 — bytes forwarded unchanged.
**Trigger:** user pressed Entry HEAT 21.5 → 22.0 °C on the NEO touchscreen.

**Provenance / transition proof:**
- B 5516 t=2183.127 → reg 136 = `00 D7` (21.5 °C, OLD)
- **B 5524 t=2186.128 → reg 136 = `00 DC` (22.0 °C, NEW)** ← FIRST B page-2 carrying the new value
- No intermediate B page-2 frame between 5516 and 5524.

## What changed in the NEO's response

| Reg | Page-1 offset / Page-2 offset | Before (B 5516 / B 5514) | After (B 5524) | Notes |
|----:|:--|:--|:--|:--|
| 136 (Entry HEAT) | page-2 data offset 20–21 | `00 D7` (21.5) | `00 DC` (22.0) | the press |
| 14 (page-1 command code) | page-1 offset 24–25 | `00 00` | `00 00` (B 5522) | **no command pulse** — confirms FINDINGS §9 |

The page-1 paired with this commit (B 5522) carries reg 14 = `0x0000`. **This is the third
independent confirmation that zone *setpoint* changes have no command pulse** — only the
value transition in page-2 itself acts as the commit signal. Distinct from zone *enable*
which pulses reg 14 = 0x40 (see `neo_response_zone_entry_enabled.md`).

## NEO page-2 response (regs 126–247) — THE PRIMARY ARTEFACT

Frame: `B 5524 t=2186.128000s +241005us 249 bytes`
First B-side page-2 carrying the new Entry HEAT value.

```
66 03 F4 00 02 00 E6 00 E6 00 D2 00 D2 00 E1 00 D2 00 D7 00 D7 00 DC 00 DC 00 E6 00 F0 00 F0 00 F0
00 E6 00 E6 01 D9 02 0A 02 0E 02 08 02 16 02 0B 02 04 01 FE 00 1E 00 1E 40 1E 40 1E 40 1E 40 1E 00
00 00 00 00 C6 00 DC 00 D2 00 CB 00 F7 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30
75 30 01 00 01 00 01 00 01 00 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 01 D9 75 30 75 30 02 3B 02 17 01 F8 02 4F 01 D3 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30
75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75
30 75 30 75 30 00 02 00 02 00 02 00 02 00 02 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 07 29 28 03 20 00 3C FF 64 00 24 7D D8
```

- Header: `66 03 F4` (slave 0x66, func 03, bytecount 0xF4 = 244)
- Data: 244 bytes covering registers 126..247 inclusive
- CRC trailer: `7D D8` (Modbus CRC-16, little-endian)

Reg-136 (Entry HEAT) at data offset 20..21 = `00 DC` = 0x00DC = 220 = **22.0 °C**.

## NEO page-1 response (regs 2–125) — paired

Frame: `B 5522 t=2185.626000s +2242000us 253 bytes`
Page-1 from the same polling cycle. Provided for context — does NOT carry the change
(reg 14 = `0x0000`, no command pulse).

```
66 03 F8 00 6E 26 23 03 2B 01 18 96 49 18 04 05 F3 00 00 80 82 59 04 00 EB 00 D0 00 00 00 00 75 30
00 D0 01 EE 00 18 00 00 00 0D 01 06 34 BF 20 02 00 60 03 F8 00 42 00 1D 75 30 75 30 03 67 00 20 00
1E 01 60 0D 41 02 C6 1B 80 75 30 75 30 00 2C 00 3E 00 54 04 7E 05 46 05 DC 00 0A 00 00 01 75 03 DE
00 00 CC EF 00 14 00 A7 02 7B 00 E6 00 EB 01 04 00 C8 00 B4 01 18 FB 02 FB 02 00 0F 00 1E D9 32 00
00 C8 23 02 26 00 32 01 2C 00 1E 00 DA 75 30 75 30 01 00 01 00 01 00 01 00 01 00 01 00 00 00 00 3F
00 3F 00 00 00 03 29 09 29 09 29 09 29 09 29 09 29 09 00 01 00 01 00 DA 00 C6 00 DC 00 D2 00 CB 00
F7 00 DA 00 DA 00 64 00 55 00 00 00 00 00 00 00 00 00 64 00 64 00 01 00 07 00 08 00 09 00 0A 00 0B
00 01 00 01 64 00 64 00 64 00 64 00 64 00 64 00 64 00 64 00 53 64
```

- Header: `66 03 F8` (slave 0x66, func 03, bytecount 0xF8 = 248)
- Data: 248 bytes covering registers 2..125 inclusive
- CRC trailer: `53 64` (Modbus CRC-16, little-endian)

## Board's confirming page-2 broadcast

Frame: `A 5530 t=2188.153000s +238001us 255 bytes`
First indoor-board broadcast after the commit. Reg 136 in the broadcast = `00 DC` ✓ matches.

```
00 10 00 7E 00 7B F6 00 00 00 E6 00 E6 00 D2 00 D2 00 E1 00 D2 00 D7 00 D7 00 DC 00 DC 00 E6 00 F0
00 F0 00 F0 00 E6 00 E6 01 D9 02 0A 02 0E 02 08 02 16 02 0B 02 04 01 FE 00 1E 00 1E 40 1E 40 1E 40
1E 40 1E 00 00 00 00 00 C6 00 DC 00 D2 00 CB 00 F7 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30
75 30 75 30 75 30 01 00 01 00 01 00 01 00 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 01 D9 75 30 75 30 02 3B 02 17 01 F8 02 4F 01 D3 75 30 75 30 75 30 75 30 75 30 75 30
75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75
30 75 30 75 30 75 30 75 30 00 02 00 02 00 02 00 02 00 02 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 07 29 28 03 20 00 3C FF 64 00 24 03 78 E9 2E
```

- Header: `00 10 00 7E 00 7B F6` (write-multiple-regs broadcast, start reg 126, qty 0x7B=123, bytecount 0xF6=246)
- Reg 136 at data offset 20..21 = `00 DC` ✓ matches NEO's report

## Notes for replay templates

`bridge::StreamingBridge::setReplayPage1Data` (250 B) and `setReplayPage2Data` (246 B) take
the bytes from offset 3 onwards of the corresponding frame, **including** the trailing CRC.

Captured CRCs (`53 64` for page-1, `7D D8` for page-2) are valid for this exact payload.
