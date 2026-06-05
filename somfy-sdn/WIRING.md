# Somfy SDN controller — wiring

This device is a **normal bus participant** on the Somfy Digital Network (SDN) RS485 bus — a
single transceiver tapped across A/B **in parallel** (do **not** cut the bus). It is **NOT** a
MITM. TX is firmware-gated by a LISTEN/ACTIVE mode; as-built it boots **ACTIVE** (sole
controller on a dedicated bus). Switch the boot default to LISTEN if a competing controller is
ever added — see `CLAUDE.md` "Bus mode & discovery".

## Parts

| Part | Notes |
|------|-------|
| Unexpected Maker **TinyC6** (ESP32-C6) | USB-C, 3.3 V logic. PlatformIO board `um_tinyc6`. |
| **3.3 V** TTL↔RS485 transceiver | **Auto-direction** (keys driver off UART TX; no DE/RE pin). 3.3 V logic only — never a 5 V MAX485 into the C6 (its RO would drive 5 V into a GPIO and kill the pin). |
| Momentary button on **GPIO0 → GND** | Provisioning / factory trigger. Already wired on field devices. |

## TinyC6 ↔ transceiver (UART1)

| TinyC6 | Transceiver | Note |
|--------|-------------|------|
| GPIO16 (board "TX") | **DI / TXD** | UART1 TX → transceiver driver input |
| GPIO17 (board "RX") | **RO / RXD** | transceiver receiver output → UART1 RX |
| 3V3 | VCC | 3.3 V only |
| GND | GND | common ground |

## Transceiver ↔ SDN bus (parallel tap)

| Transceiver | SDN bus |
|-------------|---------|
| **A** | Data+ / A |
| **B** | Data− / B |
| **GND** | bus GND (and TinyC6 GND) |

Somfy SDN over an RJ-style cable (T568B colours, from the `matter-apps` README):

| Pin | Colour | SDN role |
|-----|--------|----------|
| 1 | white/orange | RS485 + / Data+ / A |
| 2 | orange | RS485 − / Data− / B |
| 8 | brown | RS485 ground |

If frames look garbled / `err.framing` climbs in `/stats`, try swapping **A↔B**.

## Termination

On a **mid-bus parallel tap, do NOT fit the 120 Ω A–B terminator** — the bus is terminated at
its physical ends. If your transceiver module has a termination resistor, remove it.

## Power

USB-C charger / power bank, or a bus-derived 5 V buck into the 5 V pin. As with Actron, **don't**
run an active RS485 driver off a current-limited host USB port if it browns out — the failure
mode is sneaky (one-direction silence).

## Line parameters

4800 baud, **8-O-1** (odd parity), half-duplex. The firmware sets this on UART1 automatically.

## GPIO0 button

- **Long-press (~10 s):** wipe WiFi credentials → reboot into the SoftAP provisioning portal.
- **Short-press:** WINK all detected motors (a quick "which blinds is this driving?" during
  install — requires ACTIVE mode to actually transmit).
