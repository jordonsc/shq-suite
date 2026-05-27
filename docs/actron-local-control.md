# Actron A/C — Hardware & Local Control Reference

Investigation into controlling the SHQ Actron air conditioner **locally**, to escape the
unreliable cloud (`nimbus.actronair.com.au`) that the `actron_shq` HA integration depends on.

## TL;DR

- **Local whole-house control is possible** via the official **ICUNO-MOD** Modbus RS485 BMS card,
  confirmed compatible with our outdoor unit (`CRV240T`). It coexists with the NEO wall controller.
- **But per-zone temperature control is NOT exposed over Modbus** — only zone on/off. Our system
  actively uses independent per-zone target temps, so Modbus cannot fully replace the cloud/NEO for
  that feature.
- The NEO wall controller has **no local API** (cloud-only), and the NEO↔unit bus is RS485 running a
  **proprietary** protocol that is not reverse-engineered for this controller generation.

## Hardware identity

Pulled live from the cloud `lastKnownState.AirconSystem` (the HA component discards this — it only
keeps `serial` at `coordinator.py:113` and sets no `DeviceInfo`).

| Component | Value |
|-----------|-------|
| Wall controller | **NTW-1000** (NEO Touch), firmware 2.6.2.0, serial REDACTED-SERIAL |
| System type | `neo` (routes via `nimbus.actronair.com.au`) |
| Outdoor unit | **CRV240T** — 24 kW, "Inverter: Advance Series I Three Phase", serial REDACTED-SERIAL, sw 1.24 |
| Outdoor control board | "Type 150: Uno (PIC24FJ128GA308)" → UNO board family (ICUNO-MOD target) |
| Indoor unit | firmware 3.43 |
| Zones | 6 active (Living, Entry, Bedroom, Gym + Guest Rooms, Room B, Room A) via 5× wireless **NEO Zone Sensors** (BLE) |

### How to re-pull hardware/zone data

The cloud refresh token lives in `atlas:/etc/hass/.storage/core.config_entries` (world-readable).
Query it with the locally-installed `actron_neo_api` SDK:

```python
from actron_neo_api import ActronAirAPI
api = ActronAirAPI(refresh_token=<token>)
systems = await api.get_ac_systems()          # [{type, serial, description, ...}]
await api.update_status(systems[0]["serial"])
st = api.state_manager.get_status(serial)
st.last_known_state["AirconSystem"]            # models, firmware, peripherals
st.last_known_state["RemoteZoneInfo"]          # per-zone live temp + cool/heat setpoints
```

## Communication architecture

```
                    WiFi → nimbus cloud (the unreliable bit)
                     │
  ┌──────────────────┴───┐    RS485 (Cat5e, PROPRIETARY)    ┌─────────────┐
  │  NEO NTW-1000        │◄────────────────────────────────►│ Indoor board │
  │  (wall controller)   │   Actron protocol — the          │  (fw 3.43)   │
  └──────────────────────┘   "IDU Interface" in event log   └──────┬──────┘
        ▲ BLE (wireless)                                            │ wired bus
        │                                                     ┌─────┴───────┐
  5× NEO Zone Sensors                                        │ Uno outdoor  │
  (battery, per-zone temp)                                   │ board CRV240T│
                                                             └─────────────┘
```

- **NEO ↔ indoor unit**: RS485 electrically (Cat5e, 2 twisted pairs, AWG24, ≤200 m) but a
  **proprietary Actron protocol**, *not* Modbus. The status event log tags command sources as
  `GUI` (touchscreen), `Cloud`, and `IDU Interface` (the bus to the indoor board).
- **NEO ↔ zone sensors**: wireless BLE (sensors report MAC + RSSI).
- **NEO ↔ cloud**: WiFi only. The wall controller exposes **no local HTTP/LAN API** — confirmed by
  both reverse-engineered community docs (Que and Neo are cloud-only).

## Local control options evaluated

| Option | Verdict |
|--------|---------|
| **ICUNO-MOD Modbus card** | ✅ Viable, confirmed compatible. Whole-house control only (no per-zone temp). |
| ESP32 + RS485 (`awulf/Actron485`) | ❌ Targets older ESP-series controllers; does not support NEO. High risk, voids warranty. |
| Reverse-engineer NEO RS485 protocol | ❌ Not publicly documented for this generation; high effort, uncertain. |
| Improve cloud resilience | ⚠️ Interim only — doesn't remove the cloud dependency. |

## ICUNO-MOD Modbus card

Actron's outdoor Modbus RS485 BMS card (doc 9590-3013, Ver.14). Mounts on the Uno outdoor board's
**AUX 485** 4-pin port via the supplied data cable; presents Modbus RTU to a BMS.

- **Compatibility**: page 1 lists `CRV240T` under the **Advance** series → confirmed for our unit.
  UNO Outdoor Board Series = UNO/UNOPRO/UNOJR.
- **Coexists with the NEO**: configure the Uno board for **"Basic BMS Control + Wall Control"**
  (§04.07.03). The NEO keeps working; Modbus is a parallel channel. Cloud integration can stay as
  fallback.
- **Install**: inside the 3-phase outdoor electrical panel → **licensed HVAC tech only**. Card
  ~$400 AUD, sourced via a tech/trade account (not retail).

### Modbus serial parameters

| Setting | Value |
|---------|-------|
| Protocol | Modbus RTU, RS485 2-wire |
| Baud | 9600 (default), 19200, 38400, 76800, 115200 |
| Frame | 8E1 (8 data, even parity, 1 stop) default; parity also None |
| Slave address | 1–247 (DIP switch SW1) |
| Function codes | 03 (read holding), 06 (write single), 16 (write multiple) |
| Termination | 120 Ω via SW3 on first/last device |
| Max length | 1000 m @ 9600 |

### Register map (key registers)

Basic BMS control (`Set BMS Demand Mode`/reg 505 should read **1**). All 16-bit analog registers.

**Control (R/W holding):**

| Function | Reg | Notes |
|----------|-----|-------|
| On/Off | 1 | 0/1 |
| Mode | 101 | 1=Heat 2=Cool 3=Auto 4=Fan 5=Dry |
| Master setpoint | 102 | 0.1 °C (230 = 23.0 °C), range 16.0–30.0 |
| Fan mode | 4 | 1=Low 2=Med 3=High 4=Auto (Auto = Advance only, after wall self-learn) |
| Supply fan control | 105 | 0=Standard 1=Continuous |
| Turbo / Quiet | 109 / 110 | 0/1 |
| Reset filter notif | 6 | 0/1 |
| Filter timer (hr) | 7 | -1 disables |
| **Zone 1–8 on/off** | **5001–5008** | **0=Off 1=On — the ONLY per-zone control** |

**Status / monitoring (R):**

| Function | Reg |
|----------|-----|
| Alarm output | 503 |
| BMS demand mode (sanity: 1=Basic, 2=Advanced) | 505 |
| Air filter notif / run timer | 506 / 509 |
| Actual system running capacity (0.1%) | 801 |
| Room temperature (0.1 °C) | 851 |
| Outside air temperature (0.1 °C) | 852 |
| Current error code + last 5 | 900–905 |
| No comms with indoor / outdoor (1=offline) | 906 / 909 |
| Room / outside temp sensor status | 951 / 952 |
| Outdoor unit model / sw / compressor type | 1001 / 1002 / 1004 |
| Compressor 1 telemetry (running, heating, demand, speed, defrost, suction/discharge P&T, superheat, LP/HP trips) | 1101–1122 |
| Outdoor coil temp / OD fan speed | 1201 / 1215 |
| Indoor coil temp / indoor fan speed (% , RPM) | 1301 / 1311 / 1312 |
| Compressor EEV position | 1401 |
| Wall control temps 1–3 | 6001–6003 |
| Auxiliary sensor temps 1–3 | 6011–6013 |

Advanced BMS mode (reg 505 = 2) swaps the simple mode/setpoint for direct demand control
(201 system capacity demand, 202 heat request, 203 supply fan speed demand, 204 damper output).
Basic mode maps cleanly onto a HA climate entity and is the right choice.

## The zone limitation (important)

Our system runs **true per-zone individual temperature control** — each zone holds its own cool/heat
target (live snapshot at time of investigation):

| Zone | Live | Cool | Heat |
|------|------|------|------|
| Living | 20.7 | 21.0 | 23.5 |
| Entry | 19.6 | 23.0 | 22.0 |
| Bedroom | 21.7 | 20.0 | 23.0 |
| Gym + Guest Rooms | 21.2 | 21.0 | 24.0 |
| Room B | 19.9 | 21.0 | 25.0 |
| Room A | 24.1 | 23.0 | 24.0 |

`ZoneTemperatureSetpointVariance_oC = 2.0`.

**Modbus (Basic *and* Advanced) exposes zone on/off only (5001–5008).** There is **no per-zone
setpoint register** and **no per-zone temperature readback** (only the single Room Temperature 851
and wall/aux temps 6001–6003 / 6011–6013). The per-zone setpoints live on the proprietary NEO bus.

So over Modbus you get: one master setpoint + open/close each damper. You cannot set Room A to
23° while Living holds 21° via Modbus.

**Open question (needs a tech to confirm):** in "Basic BMS + Wall Control" mode, does the NEO keep
regulating each enabled zone to its stored setpoint underneath, while Modbus only drives on/off and
the master setpoint? The manual treats the system as single-setpoint, so **assume per-zone temps are
lost** unless proven otherwise.

## Decision framing

- **Set-and-forget zone temps** → Modbus for reliable system-level control (on/off, mode, fan,
  master setpoint) + let the NEO hold per-zone behaviour is a reasonable fit.
- **Actively change per-zone targets** → Modbus alone won't cut it; per-zone control would still
  depend on the cloud.

## Integration notes (if proceeding)

- The card is Modbus **RTU over RS485**; the existing HA Modbus hubs (Victron Cerbo GX) are Modbus
  **TCP**. Add an **RS485→Modbus-TCP gateway** at the outdoor unit, then add a third `modbus` hub in
  `deploy/config/ha/configuration.yaml` following the `cerbo_gx` pattern.
- Build a HA climate + sensor set on the registers above (same template/automation playbook as the
  battery sensors).

## Sources

- ICUNO-MOD Installation & Comm Guide, doc 9590-3013 Ver.14 (compatibility list + register map)
- [jxg81 homebridge-actron-que API doc](https://github.com/jxg81/homebridge-actron-que/blob/main/actron_api_documentation.md)
- [bstillitano homebridge-actron-neo API doc](https://github.com/bstillitano/homebridge-actron-neo/blob/main/actron_api_documentation.md)
- [awulf/Actron485](https://github.com/awulf/Actron485)
- [HA community: Control Actron air conditioner (ESPHome)](https://community.home-assistant.io/t/control-actron-air-conditioner/550806)
