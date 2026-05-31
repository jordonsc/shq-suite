# NEO page-1 + page-2 responses — Gym COOL @ 21.5 °C (first-transition)

**Captured:** 2026-05-31
**Trigger:** user pressed Gym COOL 21.0 → 21.5 °C on the NEO touchscreen, after the system was
explicitly switched to **cool mode** with **only Gym enabled** (reg 85 = 0x0008).
This is the missing capture for diagnosing why our `/inject` and `/respond` paths fail to
commit zone-setpoint changes for Gym specifically (whereas Living and Jordon adopted earlier).

State at capture:
- Mode: cool (reg 10 = `0x8081`)
- Active main setpoint (reg 12) = 23.0 °C
- Zones enabled (reg 85) = `0x0008` (Gym only, zone idx 3)
- Gym COOL (reg 130): 21.0 → **21.5 °C** in this transition

## Provenance

- B 10658 t=203.871 → reg 130 = `00 D2` (21.0 °C, OLD)
- **B 10666 t=206.873 → reg 130 = `00 D7` (21.5 °C, NEW)** ← saved here
- B 10664 t=206.371 is the paired page-1 from the same poll cycle

## NEO page-1 (B 10664, 253 bytes)

```
66 03 F8 00 6E 26 23 03 2B 01 18 96 49 18 04 05 F3 00 00 80 81 59 04 00 E6 00 D3 00 00 00 00 75 30
00 D3 00 25 00 48 00 00 0C 30 36 06 34 BF 10 01 00 2A 03 B6 00 BB 00 9E 75 30 75 30 01 C9 00 96 00
86 00 A3 06 22 02 EF 05 BB 75 30 75 30 00 2C 00 3E 00 54 04 7E 05 46 05 DC 00 0A 00 00 01 74 03 DE
00 00 CC EF 00 14 00 A8 01 E1 00 E6 00 EB 01 04 00 C8 00 B4 01 18 FB 02 FB 02 00 0F 00 1E D9 32 00
00 C8 23 02 26 00 32 01 2C 00 14 00 CA 75 30 75 30 01 00 01 00 01 00 01 00 01 00 01 00 00 00 00 3F
00 3F 00 00 00 08 29 09 29 09 29 09 29 09 29 09 29 09 00 01 00 01 00 CA 00 B3 00 BD 00 D3 00 BC 00
D0 00 CA 00 CA 00 00 00 00 00 00 00 37 00 00 00 00 00 64 00 64 00 01 00 07 00 08 00 09 00 0A 00 0B
00 01 00 01 64 00 64 00 64 00 64 00 64 00 64 00 64 00 64 00 F3 BF
```

Key values:
- reg 10 = `0x8081` (mode cool + quiet bit + always-set) ✓ matches board's committed mode
- reg 14 = `0x0000` (no command pulse — confirms zone setpoints have no pulse)
- reg 85 = `0x0008` (Gym only)

## NEO page-2 (B 10666, 249 bytes) — **the actual transition frame**

```
66 03 F4 00 21 00 E6 00 E6 00 D2 00 D7 00 E1 00 D2 00 D7 00 D7 00 DC 00 DC 00 E6 00 F0 00 F0 00 F0
00 E6 00 E6 01 B8 01 E4 01 F0 01 D8 01 E3 01 E0 01 DA 01 D6 40 1E 40 1E 40 1E 00 1E 40 1E 40 1E 00
00 00 00 00 B3 00 BD 00 D3 00 BC 00 D0 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30
75 30 01 00 01 00 01 00 01 00 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 01 B9 75 30 75 30 02 10 02 09 01 92 02 10 01 D1 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30
75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75 30 75
30 75 30 75 30 00 02 00 02 00 02 00 02 00 02 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 07 29 28 03 20 00 3C FF 14 00 24 11 12
```

Key values:
- reg 126 = `0x0021` (interesting — earlier captures had 0x62, 0x02, 0x20, 0x60; this one is 0x21 — bits 0+5)
- reg 130 (Gym COOL) = `0x00D7` (21.5 °C, **the change**)
- Per-zone 151–156 block: `00 1E 40 1E 40 1E 40 1E 40 1E 40 1E` — bit 14 (0x4000) clear on slot 0 (Living??), set on Gym's slot. Interesting if reg 151 represents zone 0 (which it should per FINDINGS) — Living is bit-14 clear here even though Gym was the zone changed.

## Notes for analysis

This is the third zone-setpoint change captured. Comparing this Gym-press payload against
the Living-HEAT and Entry-HEAT captures (heat-mode), plus the two random no-input baselines,
should reveal what byte/pattern signals "controller is reporting a zone setpoint change" —
since the same byte should be present in ALL three change captures but absent in the randoms.
The prior 2-vs-2 analysis missed this (only 2 change samples, both heat-mode); a 3-sample
diff with one cool-mode change should produce a stronger signal.
