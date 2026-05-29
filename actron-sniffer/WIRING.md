# Actron RS485 Sniffer — Wiring Guide

Inline pass-through tap on the NEO ↔ indoor-unit RJ45 bus so the Neo stays fully live while
the TinyC6 listens. All colours are **T568B**.

## Reference: T568B pin → colour → Actron function

- Pin 1 — White/Orange — **GND (0V)**
- Pin 2 — Orange — **+12V**
- Pin 3 — White/Green — unused
- Pin 4 — Blue — **RS485 A**
- Pin 5 — White/Blue — **RS485 B**
- Pin 6 — Green — unused
- Pin 7 — White/Brown — **GND (0V)**
- Pin 8 — Brown — **+12V**

(+12V and GND are each doubled across two conductors for current. We only tap one GND.)

## The three legs meeting at the junction

- **WALL** — the in-wall yellow Cat5 (carries 12V + bus from the indoor unit), reaching the
  junction via the female RJ45 socket.
- **NEO** — the cut Cat5 half whose RJ45 plug is in the back of the Neo; its 8 bare wires are
  at the junction.
- **ESP** — the cut Cat5 half whose bare wires go to the RS485 bridge; **only 3 of its wires
  are used**, snip the rest back.

## Step 1 — Straight-through (WALL ↔ NEO), join same colour to same colour (all 8)

Keeps the Neo fully powered and talking to the indoor unit:

- White/Orange ↔ White/Orange
- Orange ↔ Orange
- White/Green ↔ White/Green
- Blue ↔ Blue
- White/Blue ↔ White/Blue
- Green ↔ Green
- White/Brown ↔ White/Brown
- Brown ↔ Brown

## Step 2 — RS485 bridge bus side → Cat5 colour

Wire the bridge's bus-side terminals to these colours (the same conductors that pass straight
through in Step 1 — the bridge just taps them in parallel):

- bridge **A** → **Blue** (pin 4, RS485 A)
- bridge **B** → **White/Blue** (pin 5, RS485 B)
- bridge **E** (GND / 0V reference) → **White/Brown** (pin 7, GND)

Do **not** connect Orange or Brown (+12V) to the bridge — 12V on the A/B inputs will damage
it. The ESP half's other conductors are unused; cut them back so they can't short.

## Step 3 — RS485 bridge → TinyC6

- bridge **RO** → TinyC6 pin labeled **RX** (GPIO17)
- bridge **DI** → TinyC6 pin labeled **TX** (GPIO16 — unused for sniffing, wire is optional)
- bridge **DE** + **/RE** tied together → **GND** (permanent receive-only — recommended; or to
  **GPIO3**, which the firmware holds LOW)
- bridge **VCC** → TinyC6 **3V3**
- bridge **GND** → TinyC6 **GND** (same net as the White/Brown tap)

> GPIO16 ("TX") briefly carries the ROM boot-log at power-on. With DE+/RE tied to **GND** the
> transceiver can never put that on the bus — safer than gating on a GPIO that floats during
> the boot window. We never transmit, so DE has no other use.

## Step 4 — Power

- TinyC6 powered from a **USB-C charger** (power only). Don't connect the bus 12V to it.

## Pre-flight checks (before connecting the bridge)

- **Meter the wall side:** ~12V DC between Brown (pin 8, +) and White/Brown (pin 7, GND). If
  colours/polarity differ, trust the meter — a re-crimped cable can deviate.
- **Remove the 120Ω termination resistor** on the bridge module if it has one (we're tapping
  mid-bus, not capping an end).
- **A/B polarity:** if captured frames look like garbage/inverted once baud is right, swap
  Blue ↔ White/Blue at the bridge A/B. It won't hurt anything.
- Confirm the female wall socket is punched/crimped **T568B** so colours line up pin-for-pin
  with the pigtails.
