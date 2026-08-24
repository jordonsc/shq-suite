# Skopos — mmWave Sensing Node — Specification

**Status:** Draft spec (no code yet). Founding document — ratified platform decision in ledger
**shq-suite-0037** (investigation background: **shq-suite-0036**).
**Author context:** σκοπός — the watcher on the wall. A single node design for **all** mmWave
sensing roles in the estate, from trivial (corridor walk-past) to hard (shower occupancy with
water running). Unlike the ESP32 firmware twins (`actron-sniffer`, `somfy-sdn`), Skopos is a
**Raspberry Pi daemon in the dosa/nyx mould**, because the design premise is **owning the raw
radar pipeline** — which no ESP32-class module exposes.

---

## 1. Rationale

### 1.1 Why raw IQ, not a module

The 2026-08-21 investigation (ledger shq-suite-0036) established what hobbyist mmWave modules
can honestly report:

- **24 GHz parts are bandwidth-capped** (250 MHz ISM sweep → 60–75 cm true range resolution);
  their "raw" UART output is a post-DSP detection summary (C4001: one target's
  range/speed/energy; Rd-03D/LD2450: up to 3 tracked centroids). The ADC data and the
  range–Doppler matrix **never leave the chip**.
- Detection thresholds, clutter handling and target classification are **vendor firmware** —
  opaque, tuned for generic rooms, and historically painful here: the Inu-era MR24HPC1
  deployment needed two generations of ensuite tuning templates (`inu-config/radar.json`)
  and still disappointed.
- The hard scenario (§1.2) needs **signature-level discrimination** (person-in-spray vs
  spray-alone), which requires the range–Doppler data itself.

The platform decision (shq-suite-0037): **continuity beats cost** — one device deployed
everywhere, sized for the hardest room it will serve. That mandates raw IQ and an in-house
DSP pipeline.

### 1.2 The two scenarios that size the design

1. **Corridor tripwire** (easy): "someone just walked past this point", with direction,
   ~200 ms latency, immune to fans/pets-of-the-mechanical-kind. Radar break-beam, hidden in
   the wall cavity — the original product idea.
2. **Wet-area occupancy** (hard): "someone is definitely in this shower room", sensed through
   the building fabric, **while the shower is running**. Drips are trivial (persistence
   filter); the real problem is that a running shower is a wall of genuine high-velocity
   Doppler clutter, and occupancy must be separated from it by signature: a person is a
   large, slow, translating mass with gait/limb micro-Doppler; spray is a spatially-pinned,
   statistically-stationary high-velocity cone.

One pipeline, per-role classifiers (§5.4). A node's role is **config**, not hardware.

### 1.3 Alternatives rejected

| Option | Why not |
|--------|---------|
| Rd-03D / LD2450 + TinyC6 | Solves scenario 1 at $17; cannot attempt scenario 2 (tracker output only). Two-platform fleet rejected on continuity. |
| DFRobot C4001 + TinyC6 | No angle output at all; single-target summary; same ceiling. |
| TI IWR6843AOP | Better radar (3T4R, on-chip DSP, could be standalone + TinyC6). Rejected on development model: proprietary toolchain (CCS + mmWave SDK, C on an embedded R4F) makes the record→visualise→iterate loop far worse, and the shortcut (TI demo firmware + point-cloud parsing) reinstates the vendor black box. |
| 60 GHz "vitals" modules (MR60, C1001) | Wideband silicon consumed internally; processed states only — less data out than the Rd-03D. |

---

## 2. Goals / Non-goals

### In scope
- **Single node design** (hardware + daemon) for every deployment; per-node **role config**
  (`corridor` | `wet_area`) selects the detection logic.
- **Owned DSP pipeline**: SPI frame acquisition → range FFT → clutter/MTI filter → Doppler
  FFT → CFAR → feature extraction → role classifier. All in the production daemon; no vendor
  detection logic anywhere.
- **Research harness**: raw-frame recording, labelling, and visualisation (range–Doppler
  heat map, micro-Doppler spectrogram, the polar zone view) — served from the daemon itself
  so any node doubles as an instrument.
- **Events over state**: crossing events (with direction), occupancy assertions (with
  confidence), plus a continuous 0–100 **activity** metric.
- **HA integration**: `skopos` custom component, push WS coordinator + zeroconf, in the
  established suite pattern.
- **Appliance-grade hardening** of the Pi (read-only rootfs, watchdog, fast boot) — §9.
- **Site-survey capability**: the same node, on a bench PSU, measures RF paths (tile vs
  ceiling) **before any plaster is cut** — §10.

### Out of scope (for now)
- Multi-person counting / identity / trajectory analytics (the 2-element-per-axis array
  cannot resolve two targets at the same range; do not promise what the physics refuses).
- Vitals (respiration/heartbeat) — explicitly rejected as marketing-grade at this hardware
  tier.
- Still-person presence retention (the classic mmWave tuning pit). Roles are motion-centric;
  a washing/showering person is never radar-still. If a "reading in an armchair" role is ever
  wanted, it is a new role with its own spec revision, not a tweak.
- Battery/PoE power variants. Nodes are mains-fed via an electrician-installed Meanwell PSU.
- Outdoor use.

---

## 3. Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                       Home Assistant                          │
│   custom_components/skopos  (config flow + WS coordinator)    │
│   binary_sensor.skopos_<node>_*   sensor.*_activity   events  │
└──────────────────────────────┬───────────────────────────────┘
                               │ ws://<node>:8768   (push events/state)
                               │ http://<node>:8080 (debug + research UI + OTA-ish deploy)
┌──────────────────────────────▼───────────────────────────────┐
│                skopos daemon (Rust, systemd user svc)          │
│  ┌─────────┐ ┌──────────┐ ┌────────────┐ ┌────────────────┐   │
│  │ WS API  │ │ HTTP API │ │ recorder   │ │ research UI    │   │
│  │ (8768)  │ │ + stats  │ │ (.sk8 raw) │ │ (static + live)│   │
│  └────┬────┘ └────┬─────┘ └─────┬──────┘ └───────┬────────┘   │
│       └───────────┴──────┬──────┴────────────────┘            │
│                  ┌───────▼────────┐                           │
│                  │ role classifier │  corridor | wet_area      │
│                  ├────────────────┤                           │
│                  │ feature extract │  tracks, activity, m-D    │
│                  ├────────────────┤                           │
│                  │ CFAR + cluster  │                           │
│                  ├────────────────┤                           │
│                  │ range FFT → MTI │                           │
│                  │ → Doppler FFT   │  (per RX channel + angle) │
│                  ├────────────────┤                           │
│                  │ SPI acquisition │  frame IRQ, FIFO drain    │
│                  └───────┬────────┘                           │
└──────────────────────────┼───────────────────────────────────┘
                           │ SPI + IRQ GPIO (40-pin header)
                  ┌────────▼─────────┐
                  │ DreamHAT+ Radar   │  Infineon BGT60TR13C
                  │ 58–63.5 GHz FMCW  │  1 TX / 3 RX (L-array)
                  │ 3× ADC 12-bit     │  FoV ≈ 40° H × 65° V
                  └───────────────────┘
```

Host: **Raspberry Pi 5** (kiosk-fleet continuity), headless, hardened (§9). One node = one
Pi + one HAT; the daemon is the only workload.

---

## 4. Hardware & node build

| Part | Notes |
|------|-------|
| Raspberry Pi 5 (4 GB is ample) | Same platform as the kiosk fleet — shared spares, tooling, muscle memory. Pi 4B is DreamHAT+-compatible if a 5 is ever short. |
| **DreamHAT+ Radar** (Pimoroni/DreamBoards, ~AU$225) | Infineon **BGT60TR13C**: 58–63.5 GHz FMCW, 1 TX / 3 RX antennas in an L (azimuth + elevation phase pairs), 3 ADC channels, 12-bit, up to 4 MSps, SPI to the Pi. Radar draw ~0.5 W. |
| microSD (A2) or NVMe base | Rootfs is read-only overlay (§9), so SD endurance is a non-issue; NVMe optional. |
| Passive heatsink case | Daemon load is a few % of one core (§6.4); a sealed cavity in summer is fine with passive metal. No fan — a fan in the wall would be both a failure point and a Doppler-visible joke. |
| Meanwell 5 V supply (electrician-installed) | Headless Pi 5 with no USB peripherals runs happily on a dumb 5 V/3 A+ feed; spec **5 V/5 A** rail headroom anyway. Feed via USB-C or the header 5 V pins per the installer's preference. |
| **Fleet spares: N+2 DreamHAT+ boards** | DreamBoards is a small vendor; a wall-embedded fleet must not depend on their future stock. The BGT60TR13C chip itself is mainline Infineon with other carrier boards in existence — a carrier swap is a driver-layer change, not a redesign (§6.1 isolates acquisition). |

**Siting rules (from the physics — ledger shq-suite-0036):**
- FoV is **~40° H × 65° V** — much narrower than the 100° hobby modules. Perfect for a
  tripwire (a narrow beam *is* the break-beam); it makes placement a per-room design step.
- Plasterboard is near-transparent at 60 GHz; **foil-backed insulation, sarking, and metal
  studs are hard blockers** — verify the cavity before committing a location.
- **Wet tile is hostile**: water is strongly absorptive/reflective at 60 GHz and a sheeting
  wet film sits exactly in the path. For showers, the **ceiling mount** (dry plasterboard,
  person at 1–2 m, spray cone below and range-separated) is the expected answer; the
  behind-tile option must survive a §10 survey measurement before it is ever built.
- Mount the radar face parallel to and a few cm off the back of the lining board to keep
  near-field reflections out of the first range bins (mask the first ~2 bins regardless).
- Multiple nodes: 60 GHz barely penetrates repeated walls and chirps are unsynchronised, so
  inter-node interference is a non-issue at estate scale.

---

## 5. Radar & DSP reference

This section is the "protocol reference" equivalent: the signal chain we own. Values marked
**⚠ tune** are research-phase deliverables (§10), not constants.

### 5.1 Frame acquisition

- The BGT60TR13C runs autonomous chirp sequences into an on-chip FIFO; the Pi drains frames
  over SPI on an IRQ. The DreamRF Python driver is the **reference implementation** for
  register bring-up; the Rust `acquire` module ports its sequence (§6.1).
- **Starting chirp config (⚠ tune):** 64 chirps/frame, 128 samples/chirp, 3 RX,
  **20 frames/s**. ≈ 49 KB/frame ≈ 1 MB/s SPI — trivial. Yields ≈ 3–4 cm range bins over a
  ~0.15–7 m span, velocity bins ≈ 0.1–0.2 m/s over ±3–4 m/s. Per-role configs may differ
  (corridor wants frame rate; wet_area wants velocity resolution).

### 5.2 Pipeline stages

1. **Range FFT** per chirp per RX (window: Hann) → complex range bins.
2. **Clutter/MTI filter**: per-range-bin slow-time high-pass (subtract a running mean /
   first-order IIR). Removes the static world; this is also why Skopos never claims
   still-person presence.
3. **Doppler FFT** across the frame's chirps per range bin → **range–Doppler map** per RX.
4. **Angle**: phase comparison across the RX pairs at detected cells → azimuth (+ coarse
   elevation from the L's vertical pair). Angle *accuracy* a few degrees for one clean
   target; angular *resolution* effectively nil — honest zone width **15–20°**. Never
   promise separation of two same-range targets.
5. **CFAR** (cell-averaging, guard cells) on the combined map → detections; cluster adjacent
   cells → blobs `{range, velocity, azimuth, energy, extent}`.
6. **Tracking**: lightweight α–β tracker over blobs → tracks with position history and
   displacement. (Ours, so its failure modes are ours to see — unlike the Rd-03D's.)
7. **Features** per frame/track: total moving energy (log-scaled), per-band energy split,
   track displacement, velocity centroid + spread (micro-Doppler width), persistence.

### 5.3 The `activity` metric

A continuous **0–100** score published every frame-batch: `log(energy)` normalised against a
per-node noise floor (learned at install, re-learned nightly), gated by persistence (N of M
frames) and a velocity floor. This is the honest "something is genuinely moving that isn't a
dripping tap" number — and the primary debugging/tuning surface.

### 5.4 Role classifiers

**`corridor` — crossing detector:**
- Trigger: a track whose **displacement** exceeds `min_travel` (⚠ tune, ~0.5 m) within
  `travel_window` (~2 s), with the Doppler centroid flipping approach→recede (or v.v.) near
  closest approach.
- Output: `crossing` event with `direction` (L→R / R→L from azimuth progression, or
  toward/away for end-on mounting), `speed_estimate`, `confidence`.
- Fan immunity is **structural**: a fan's track never travels. No energy threshold can fire
  this role; only displacement can.

**`wet_area` — occupancy-despite-spray:**
- Drips: killed by persistence + minimum extent. Never reaches the classifier.
- Spray-alone signature: high-velocity, **spatially pinned** cone (fixed range/azimuth,
  stationary statistics over seconds). Learned per-node during commissioning (run the shower
  with the room empty; record; fit the mask).
- Occupancy: a **large, low-velocity blob whose centroid translates between bins**, with
  gait/limb micro-Doppler broadening — evaluated against the spray mask.
- Output: `occupied` binary with hold-time hysteresis + `confidence`; falls back to a
  conservative `activity`-only mode if the spray mask is unfit (surfaced as a health flag,
  never silently).
- If hand-built features prove insufficient, the recorded corpus (§6.2) trains a small
  classifier — still ours, still inspectable. **⚠ research-phase deliverable.**

---

## 6. Daemon design (Rust)

### 6.1 Layout

```
skopos/
  Cargo.toml            # tokio, tracing, serde, rustfft, spidev/rppal, axum (HTTP), tungstenite (WS)
  build-rpi.sh          # cross → aarch64 RPi build, per suite convention (CROSS_CONTAINER_ENGINE=podman)
  src/
    main.rs             # boot wiring: config → acquire task → dsp task → servers
    config.rs           # /etc/skopos/config.yaml: role, geometry, chirp cfg, masks, thresholds
    acquire.rs          # SPI/IRQ frame acquisition (BGT60TR13C bring-up ported from DreamRF driver)
                        #   — the ONLY hardware-facing module; a future carrier swap lands here
    dsp/
      range.rs          # range FFT + window
      clutter.rs        # MTI filter
      doppler.rs        # Doppler FFT → range–Doppler map
      angle.rs          # RX-pair phase → azimuth/elevation
      cfar.rs           # CFAR + clustering
      track.rs          # α–β tracker
      features.rs       # activity metric + per-track features
    roles/
      corridor.rs       # crossing detector
      wet_area.rs       # spray mask + occupancy classifier
    recorder.rs         # raw-frame capture sessions → .sk8 files (research corpus)
    ws_api.rs           # WS server :8768 — push state/events, commands, heartbeat
    http_api.rs         # :8080 — /stats(.json), /health, live map endpoints, /record, research UI
    version.rs          # SKOPOS_VERSION semver (bump every deploy — root CLAUDE.md convention)
  web/                  # research UI: polar zone view, range–Doppler heat map, spectrogram,
                        #   labelling controls (static files + live WS/JSON feeds; argus/web pattern)
  research/             # Python harness: .sk8 readers, offline analysis, threshold fitting,
                        #   (optional) classifier training. Never deployed to nodes.
  test/                 # host unit tests: DSP stages against golden .sk8 fixtures
  CLAUDE.md             # to be written when code lands
```

**DSP purity rule** (the `sdn.{h,cpp}` lesson, transposed): everything under `dsp/` and
`roles/` is pure — frames in, detections out, no I/O, no clock reads — so the entire pipeline
is unit-testable on any host against **golden recordings**, and a field bug is reproducible
on the bench by replaying the `.sk8` that exhibited it. `cargo test` runs the full pipeline
against fixtures; no radar hardware needed.

### 6.2 The `.sk8` recording format

Raw frames + config header + optional label track, written by `recorder.rs`, read by the
research harness and the test suite. Recordings are the project's crown jewels: every tuning
decision, spray mask, and classifier traces to labelled `.sk8` files. Store the corpus on
atlas (not on nodes); nodes hold only a bounded ring of recent frames for incident replay.

### 6.3 Config (per node)

```yaml
node: shower-ensuite-1
role: wet_area            # corridor | wet_area
geometry:
  mount: ceiling          # wall | ceiling
  height_m: 2.4
chirp: default            # named profile, or inline overrides
masks:
  near_bins: 2            # always masked
  spray: spray-2026-09.mask   # wet_area: fitted at commissioning
thresholds: {}            # role-specific ⚠ tune values; empty = profile defaults
```

Estate-specific values (which rooms, addresses, masks) live in the gitignored deploy config /
`shq-suite-config`, per the suite's secrets convention.

### 6.4 Runtime budget

Frame maths at the §5.1 starting config: 192 × 128-pt FFTs + 384 × 64-pt FFTs per frame at
20 fps ≈ low single-digit % of one Pi 5 core in Rust. RAM: tens of MB including the replay
ring. Thermals: passive, sealed-cavity safe. If a future role wants 10× the frame rate, the
budget holds; the SPI link and FIFO are the eventual ceiling, not the CPU.

---

## 7. APIs

### 7.1 HTTP (:8080) — debug + research surface

| Endpoint | Purpose |
|----------|---------|
| `GET /` | research UI (web/): live polar zones, range–Doppler map, spectrogram, labelling |
| `GET /stats` / `GET /stats.json` | one-line / JSON status: role, fw semver, uptime, frame rate, activity, track count, CFAR hit rate, clock/health counters — the fleet-sweep surface, per suite convention |
| `GET /health` | machine liveness (systemd watchdog + HA availability probes) |
| `GET /map` | current range–Doppler map (JSON/binary) — feeds the live UI |
| `GET /tracks` | current track table |
| `POST /record` | start/stop a raw capture session (`{label, duration_s}`) → `.sk8` |
| `POST /mask/spray/fit` | wet_area commissioning: fit the spray mask from a just-recorded empty-room-shower-running session |
| `POST /reload` | re-read config without restart |

Mutating endpoints are `POST` (the somfy-sdn REST discipline). Security: LAN-open, the
estate's restricted-network model — same stance as every other suite device.

### 7.2 WS (:8768) — HA runtime surface

Same shape as the actron/somfy WS APIs: snapshot on connect, push on change, ~10 s heartbeat
(**unsigned elapsed-time comparisons throughout** — the shq-suite-0034 lesson is portable to
any timing code, not just ESP32s).

**Outgoing `state`:**
```json
{ "type": "state", "data": {
    "node": "corridor-hall-1", "role": "corridor", "fw": "0.1.0",
    "activity": 12, "occupied": false, "tracks": 0, "health": "ok" } }
```

**Outgoing `event`** (the point of the corridor role — events, not polled state):
```json
{ "type": "event", "data": {
    "kind": "crossing", "direction": "ltr", "speed": 1.2, "confidence": 0.94, "ts": "…" } }
```

**Incoming:** `record`, `fit_spray_mask`, `reload`, `identify` (blinks the Pi LED — the
CTRL_WINK of this platform), all ack/error correlated by `id` as per the suite pattern.

---

## 8. Home Assistant component (`skopos`)

`home-assistant/custom_components/skopos/` — config flow (host + port 8768) **+ zeroconf**
(`_skopos._tcp`, TXT `id=<machine-id>`, self-healing host rewrite — the somfy_sdn pattern
verbatim). Coordinator ported from `actron_mitm_controller`/`somfy_sdn` (push WS, layered
reconnect, close-before-reconnect).

**Entities per node:**
- `binary_sensor.<node>_occupied` (role `wet_area`; device_class `occupancy`)
- `sensor.<node>_activity` (0–100, both roles)
- Crossing events → fired on the HA event bus (`skopos_crossing` with direction/speed) **and**
  a short-pulse `binary_sensor.<node>_crossing` for dumb automations.
- Diagnostics: `sensor` frame rate + health, `binary_sensor` spray-mask-fit (wet_area).
- `button.<node>_identify`.

Availability follows the WS heartbeat (~30 s), per the coordinator pattern.

---

## 9. Node hardening (Pi-as-appliance)

- **Read-only rootfs** (overlayfs); persistent state (masks, noise floor, replay ring) on a
  dedicated small RW partition, synced with care.
- **Hardware watchdog** (`bcm2835_wdt` + systemd `RuntimeWatchdogSec`); the daemon pets it
  via `sd_notify` only while the acquisition loop is live — a wedged SPI bus reboots the node.
- **systemd user service** under the `shq` user with linger (the suite's standard),
  `Restart=always`, boot-to-detecting **< 30 s**.
- **No OS package drift**: image built once, updated deliberately via the deploy tool; no
  unattended-upgrades on nodes.
- Deploy via `./setup skopos` (rsync binary + web/ + config, restart unit) — the dosa/nyx
  flow; targets in `deploy/config/deployment/skopos.yaml` (gitignored, mirrored in
  `shq-suite-config`).
- **Versioning:** bump `SKOPOS_VERSION` every deploy; `/stats` reports it (root CLAUDE.md
  convention — it is the deploy-took confirmation).

---

## 10. Phasing

1. **Bench** — order 1× Pi 5 + DreamHAT+ now (fleet N+2 only after phase 2 signs off).
   Deliverables: `acquire.rs` bring-up (DreamRF driver as reference), pipeline through CFAR,
   the research UI, `.sk8` recorder, corridor classifier proven against bench recordings of
   real walk-pasts + a deliberately adversarial pedestal fan.
2. **Site survey** — the node on a bench PSU, held against real surfaces. Required
   measurements, each a labelled `.sk8`: (a) through-plasterboard reference; (b) shower
   ceiling path, water off/on, room empty; (c) **behind-tile path, wet, room empty** — this
   is the go/no-go for any tile-mounted node; (d) person-in-shower, water running (the
   wet_area positive class); (e) the office chair + fans scenario that started all this.
   **No plaster is cut before this phase signs off placement per room.**
3. **Productionise** — wet_area classifier from the phase-2 corpus, hardening image, HA
   component, deploy tooling. Electrician's single visit installs all approved locations.
   Fleet order (N+2) placed at phase start.

---

## 11. Risks / open items

1. **60 GHz through wet tile — unproven.** THE fleet-critical unknown; resolved empirically
   in phase 2. Ceiling mount is the strong fallback and may simply be the answer.
2. **DreamRF driver maturity.** Small vendor, young Python driver; the register bring-up
   port in `acquire.rs` may need Infineon datasheet spelunking. Mitigation: the driver is
   open source and the chip is documented; acquisition is isolated in one module.
3. **Vendor continuity.** N+2 spares; chip is mainline Infineon; carrier swap = `acquire.rs`.
4. **Spray-mask drift** (new shower head, re-grouted wall): surfaced by the mask-fit health
   flag + conservative fallback, refit via one HTTP call. Not silent.
5. **FoV siting discipline** — every room needs a placement drawing before its hole exists
   (phase 2 output).
6. **Wet-area classifier difficulty is unbounded until measured.** If person-in-spray
   separation proves weaker than expected at ceiling geometry, the honest fallbacks are
   (a) occupancy = activity + door/entry fusion in HA, or (b) accepting reduced confidence
   during active spray — both to be decided on phase-2 data, recorded in the ledger.
7. **Naming collision check**: port 8768 is free (8765 display / 8766 dosa / 8767 actron+somfy
   / 8770 argus); `_skopos._tcp` is unclaimed.

---

## 12. References

- Ledger **shq-suite-0036** (capability investigation: module limits, physics, catalogue) and
  **shq-suite-0037** (platform decision, alternatives rejected).
- Suite patterns: `somfy-sdn/SPEC.md` (spec shape, WS/HTTP/zeroconf conventions, heartbeat
  lessons), `actron-sniffer/` (WS coordinator origin), `dosa`/`nyx` (Pi daemon + cross deploy),
  `argus/web` (served live web UI).
- Infineon **BGT60TR13C**: <https://www.infineon.com/part/BGT60TR13C> (datasheet + app notes
  for register-level bring-up).
- DreamHAT+ Radar: <https://shop.pimoroni.com/en-us/products/dream-hat-plus-radar>; DreamRF
  GitHub org (reference Python driver).
- Prior art in-estate: `inu-py/src/inu/hardware/mmwave/mr24hpc1.py` +
  `inu-config/radar.json` (the module-era tuning record — the cautionary tale).
- FMCW processing background: Infineon radar application notes (range/Doppler FFT, CFAR);
  TI mmWave training series (concepts transfer; toolchain does not).
