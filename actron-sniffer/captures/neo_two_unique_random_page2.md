# NEO page-2 responses — two random unique payloads

**Captured:** post-FreeRTOS-task firmware, fw="May 30 2026 23:36:12"
**Device state at capture:** bridge=passthru, B.mod=0, A.mod=0; system in heat mode, no user input.
**Purpose:** baseline pair of "natural drift" between consecutive same-type NEO responses.
Captured fresh (no historical lookback); polled until byte-different.

Both frames are B-side page-2 responses (`66 03 F4`, bytecount 0xF4 = 244 data bytes, 249 B
total incl. CRC). Captured **3.000 s apart** — one Modbus polling cycle.

## FIRST

Frame: `B 6196 t=2438.213s 249 bytes`

```
66 03 F4 00 20 00 E6 00 E6 00 D2 00 D2 00 E1 00 D2 00 D7 00 D7 00 DC 00 DC 00 E6 00 F0 00 F0 00 F0
00 E6 00 E6 01 D2 02 05 02 0B 02 06 02 14 02 09 02 01 01 FB 40 1E 00 1E 40 1E 40 1E 40 1E 40 1E 00
00 00 00 00 C7 00 DC 00 D2 00 CD 00 F8 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30
75 30 01 00 01 00 01 00 01 00 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 01 D2 75 30 75 30 02 39 02 16 01 F8 02 4C 01 D1 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30
75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75
30 75 30 75 30 00 02 00 02 00 02 00 02 00 02 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 07 29 28 03 20 00 3C FF 64 00 24 FC 0C
```

CRC: `FC 0C`

## SECOND

Frame: `B 6204 t=2441.213s 249 bytes` (Δt = 3.000 s)

```
66 03 F4 00 60 00 E6 00 E6 00 D2 00 D2 00 E1 00 D2 00 D7 00 D7 00 DC 00 DC 00 E6 00 F0 00 F0 00 F0
00 E6 00 E6 01 D3 02 06 02 0B 02 06 02 14 02 09 02 01 01 FB 40 1E 00 1E 40 1E 40 1E 40 1E 40 1E 00
00 00 00 00 C7 00 DC 00 D2 00 CD 00 F8 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30
75 30 01 00 01 00 01 00 01 00 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 01 D2 75 30 75 30 02 39 02 15 01 F8 02 4C 01 D1 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30
75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75
30 75 30 75 30 00 02 00 02 00 02 00 02 00 02 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 07 29 28 03 20 00 3C FF 64 00 24 47 0F
```

CRC: `47 0F`

## Byte-level diff (FIRST → SECOND)

| Data offset | Register | Side | Before | After | Notes |
|------------:|---------:|:-----|:-------|:------|:------|
| 1   | 126 | lo | `20` | `60` | board status flag (toggles between values; not a setpoint) |
| 35  | 143 | lo | `D2` | `D3` | per-zone live drift (FINDINGS: regs 143–150 are slow drift, likely fine-scale per-zone temp) |
| 37  | 144 | lo | `05` | `06` | same drift block |
| 139 | 195 | lo | `16` | `15` | board telemetry / live counter (FINDINGS §7: "suspect/secondary, ignore in diffs") |
| 247 | CRC lo | — | `FC` | `47` | naturally different due to payload differences |
| 248 | CRC hi | — | `0C` | `0F` | — |

**4 data bytes** drifted across one 3-second polling cycle. All in known drift / status
registers. Zone setpoints (127–142), zone enable (85, page-1), and mode (10, page-1) are
not in this frame's diff scope (page-1 territory or fully stable here).

This pair is the "no user input" baseline — any byte position NOT changing here is a
candidate "static" register that would only move under user-driven commands.
