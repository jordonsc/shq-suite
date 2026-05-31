# NEO page-1 + page-2 responses — Living HEAT @ 22.0 °C

**Captured:** 2026-05-30 (post-FreeRTOS-task firmware, fw="May 30 2026 23:36:12")
**Device state at capture:** bridge=passthru, B.mod=0, A.mod=0 — bytes forwarded unchanged.
**Trigger:** user pressed Living HEAT 22.5 → 22.0 °C on the NEO touchscreen.

This is a clean, CRC-valid pair of NEO func-03 responses immediately after the press committed.
Both frames captured by the bridge at full length with no mid-frame splits — confirmation
that the new pump task (`bridgeTask`) is forwarding cleanly.

Suitable as a known-good template for `bridge::StreamingBridge::setReplayPage1Data` /
`setReplayPage2Data`. The 250-byte / 246-byte arrays expected by the bridge are exactly the
3rd byte onward (`F8 …` for page 1 / `F4 …` for page 2) including the trailing 2-byte CRC.

## State at capture

- Mode: heat (reg 10 low nibble = 2)
- Active main setpoint: 21.5 °C (reg 12 = 0x00D7)
- Zone HEAT array (reg 135–142):
  - Living    = **22.0** °C (0x00DC) ← just changed from 22.5
  - Entry     = 22.0 °C (0x00DC)
  - Bedroom   = 23.0 °C (0x00E6)
  - Gym+Guest = 24.0 °C (0x00F0)
  - Sal       = 24.0 °C (0x00F0)
  - Jordon    = 24.0 °C (0x00F0)
  - Z7 (unused) = 23.0 °C (0x00E6)
  - Z8 (unused) = 23.0 °C (0x00E6)
- Zone cool array (reg 127–134): 23.0 23.0 21.0 21.0 22.5 21.0 21.5 21.5
- Live zone temps (reg 94–101, page 1): 23.5 / 21.5 / 21.4 / 21.8 / 21.5 / 21.7 / 21.5 / 21.5

## Page-1 response (regs 2–125)

Frame: `B 2905 t=1202.849000s +2242000us 253 bytes`
First full B-side page-1 response of the polling cycle in which the press committed.

```
66 03 F8 00 6E 26 23 03 2B 01 18 96 49 18 04 05 F3 00 00 80 82 59 04 00 EB 00 D7 00 00 00 00 75 30
00 D7 02 1C 00 18 00 00 17 38 25 05 34 BE 20 02 00 53 03 8E 00 46 00 27 75 30 75 30 03 59 00 1F 00
12 01 96 0D 78 02 E6 17 C8 75 30 75 30 00 2C 00 3E 00 54 04 7E 05 46 05 DC 00 0A 00 00 01 76 03 DE
00 00 CC EF 00 14 00 8F 02 0F 00 E6 00 EB 01 04 00 C8 00 B4 01 18 FB 02 FB 02 00 0F 00 1E D9 32 00
00 C8 23 02 26 00 32 01 2C 00 15 00 D7 75 30 75 30 01 00 01 00 01 00 01 00 01 00 01 00 00 00 00 3F
00 3F 00 00 00 01 29 09 29 09 29 09 29 09 29 09 29 09 00 01 00 01 00 D7 00 C4 00 DA 00 D2 00 C5 00
F4 00 D7 00 D7 00 64 00 00 00 00 00 00 00 00 00 00 00 64 00 64 00 01 00 07 00 08 00 09 00 0A 00 0B
00 01 00 01 64 00 64 00 64 00 64 00 64 00 64 00 64 00 64 00 9C DA
```

- Header: `66 03 F8` (slave 0x66, func 03 read holding regs, bytecount 0xF8 = 248)
- Data: 248 bytes covering registers 2..125 inclusive (byte 0..1 = reg 2 hi/lo, etc.)
- CRC trailer: `9C DA` (Modbus CRC-16, little-endian)

## Page-2 response (regs 126–247)

Frame: `B 2907 t=1203.351000s +241008us 249 bytes`
First B-side page-2 response carrying the NEW Living HEAT value of 22.0 °C.

```
66 03 F4 00 62 00 E6 00 E6 00 D2 00 D2 00 E1 00 D2 00 D7 00 D7 00 DC 00 DC 00 E6 00 F0 00 F0 00 F0
00 E6 00 E6 01 DE 02 0C 02 10 02 0A 02 19 02 0E 02 07 02 02 00 1E 40 1E 40 1E 40 1E 40 1E 40 1E 00
00 00 00 00 C4 00 DA 00 D2 00 C5 00 F4 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30
75 30 01 00 01 00 01 00 01 00 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 01 DF 75 30 75 30 02 3B 02 18 01 F8 02 54 01 D8 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30
75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75
30 75 30 75 30 00 02 00 02 00 02 00 02 00 02 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 07 29 28 03 20 00 3C FF 64 00 24 7F 9E
```

- Header: `66 03 F4` (slave 0x66, func 03, bytecount 0xF4 = 244)
- Data: 244 bytes covering registers 126..247 inclusive
- CRC trailer: `7F 9E` (Modbus CRC-16, little-endian)

Reg-135 (Living HEAT) is at data offset 18..19 (bytes `00 DC` — hi byte first per Modbus
big-endian — = 0x00DC = 220 = **22.0 °C**).

## Board's confirming broadcast (regs 126–248, addr 0x00 broadcast)

Frame: `A 2921 t=1208.376000s +238022us 255 bytes`
Indoor board's first authoritative broadcast of page-2 state after the commit. Reg 135 here
matches the NEO's report → confirms the value was committed cleanly.

```
00 10 00 7E 00 7B F6 00 00 00 E6 00 E6 00 D2 00 D2 00 E1 00 D2 00 D7 00 D7 00 DC 00 DC 00 E6 00 F0
00 F0 00 F0 00 E6 00 E6 01 DF 02 0D 02 10 02 0A 02 19 02 0E 02 07 02 02 00 1E 40 1E 40 1E 40 1E 40
1E 40 1E 00 00 00 00 00 C4 00 DA 00 D2 00 C5 00 F4 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30
75 30 75 30 75 30 01 00 01 00 01 00 01 00 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 01 DE 75 30 75 30 02 3B 02 18 01 F8 02 54 01 D8 75 30 75 30 75 30 75 30 75 30 75 30
75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75
30 75 30 75 30 75 30 75 30 00 02 00 02 00 02 00 02 00 02 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 07 29 28 03 20 00 3C FF 64 00 24 03 78 BB 69
```

- Header: `00 10 00 7E 00 7B F6` (write-multiple-regs broadcast to addr 0x00, start reg 0x7E=126,
  qty 0x7B=123, bytecount 0xF6=246)
- Reg 135 = data offset 7+18..19 = `00 DC` = 22.0 °C ✓ matches NEO

## Notes for replay templates

`bridge::StreamingBridge` expects payloads **without** the 3-byte slave/func/bytecount header
but **with** the 2-byte CRC trailer:

- `kReplayPage1Bytes = 250` → bytes 3..252 of the page-1 frame above (data + CRC)
- `kReplayPage2Bytes = 246` → bytes 3..248 of the page-2 frame above (data + CRC)

The captured CRC bytes (`9C DA` for page-1, `7F 9E` for page-2) are valid for this exact
payload — feed back unchanged in replay mode and the indoor board will accept the frame.
