# Phase 4 — Kiosk Takeover & the Sci-Fi HUD

> Sub-spec of [Argus master plan](./00-master.md). Depends on **Phase 2** (`CaseState` channel +
> still files) and reuses **nyx** for the takeover.
>
> **Status: 📝 NOT STARTED.**
>
> **Goal:** when the alarm fires, every wall kiosk takes over with the **vector sci-fi HUD** showing
> the live assessment and the best identifying stills as they register; on disarm, kiosks return to
> their dashboards (optionally flashing the green **AUTHORISED** state first).

## Context

The HUD is a **web app Argus serves** and the kiosks display via `nyx`'s existing CDP `navigate`.
The chrome is **drawn**, not raster — `docs/concept/alarm.png` (red) and `authorised.png` (green) are
the **same frame** in two hues, so build **one parametric component** themed by `--hud:
crimson|emerald`. The PNGs are the visual spec; the only raster content is the live camera
stills/mugshots dropped into the drawn frame.

## Scope

**In scope:**
- `argus/web/` — static HUD app (HTML/CSS/SVG/Canvas/JS), authored from the concept PNGs, themed by
  a CSS custom property.
- Argus **HTTP server** (`axum`): serve `argus/web/` at `/alarm`, serve case stills at
  `/stills/<id>.jpg`.
- Argus **kiosk WS** (`/kiosk`): broadcast `CaseState` JSON to connected HUDs; they render live.
- **Takeover orchestration:** on trigger, flip every kiosk to the alarm URL via the
  `shq_display.navigate` HA service; on disarm, navigate back to each kiosk's dashboard.
- The **AUTHORISED** (green) state shown on resident disarm / all-clear.

**Out of scope:**
- A Chronos-style layer-shell overlay (instant stateless dismissal) — deferred; navigate-reload is
  fine for a deliberate alarm. Note it as the future enhancement.

## Implementation

### 1. The HUD web app (`argus/web/`)
- **Frame as vector:** corner brackets, segmented border, registration crosshairs, the warning
  triangle, the header band (BLACKROSE GLOBAL COMMAND logo / live timer / CLEARANCE badge) — all
  SVG/CSS, sized to the portrait kiosk viewport, resolution-independent.
- **Theming:** one component, `:root { --hud: crimson }` (alarm) vs `--hud: #34d058` (authorised);
  the header word swaps ALARM MODE ⇄ AUTHORISED. Match the concept palette/typography (audit the
  PNGs for exact hues).
- **Content panel:** the large bordered centre area holds (a) the current camera still(s), (b)
  **intruder cards** that slide in as `intruders[]` populate (mugshot + descriptors + confidence +
  location/activity), (c) a **ticker** rendering the `timeline`/`summary` (type-on effect).
- **Motion:** a radar/scan sweep, a pulsing alert border keyed to `threat_level`, card slide-ins.
  Keep it GPU-cheap (CSS transforms / a single Canvas) — these run on RPi 5 kiosks.
- **WS client:** connect `ws://atlas:<port>/kiosk`, apply each `CaseState` update; reference stills
  by `/stills/<id>.jpg`. Reconnect on drop. Render the green AUTHORISED state when
  `status == Cleared`/authorised disarm.

### 2. Argus HTTP + WS server (`src/web/mod.rs`)
- `axum`: static route `/alarm` → `argus/web/`; `/stills/:id` → the case-dir still bytes (from Phase
  2's case dir); `/kiosk` WS → subscribe to the `CaseState` channel and push JSON on each update.
- Bind on the LAN (atlas) so kiosks can reach `http://atlas:<port>/alarm`.

### 3. Takeover orchestration (`src/out/kiosks.rs`)
- Config: `kiosks[]` with each kiosk's HA `shq_display` target + its **dashboard URL** (to return to).
- On trigger: for each kiosk, call the HA service `shq_display.navigate` → `http://atlas:<port>/alarm`.
- On disarm: navigate each kiosk back to its dashboard URL.
- **nyx interaction to verify:** kiosks in `idle_mode: clock` may have the Chronos overlay up (it
  sits *above* Chrome), and a sleeping kiosk's backlight is off — a bare `navigate` won't be visible.
  Ensure the screen is woken and any Chronos overlay killed on takeover. If `shq_display.navigate`
  doesn't already imply wake, either call `wake`/`set_display true` first **or** add a small nyx
  change so `navigate` wakes + kills the overlay. **Flag this as a nyx dependency** and record what
  was needed in Deviations.

## Config additions
```yaml
web:
  bind: "0.0.0.0:8770"      # HUD + stills + kiosk WS
  public_base: "http://atlas:8770"
kiosks:
  - { ha_target: "kiosk03", dashboard_url: "http://athena/lovelace/kitchen" }
  # ...all kiosks, with their return dashboards
```

## Verification (definition of done)
1. Trigger → every kiosk wakes and shows the red ALARM HUD; the ticker and stills update live as
   `CaseState` evolves; intruder cards slide in with mugshots + descriptors.
2. A clock-mode kiosk and an asleep kiosk both take over correctly (Chronos killed / backlight on).
3. Disarm → AUTHORISED (green) confirmation (on resident disarm), then kiosks return to their
   dashboards.
4. Runs smoothly on the RPi 5 kiosks (no jank).

## References
- `nyx/CLAUDE.md` (CDP, the `navigate` WS command), `home-assistant/CLAUDE.md` (`shq_display.navigate`
  service), `chronos/CLAUDE.md` (overlay/wake interaction), ledger `shq-suite-0001`.
- `docs/concept/alarm.png`, `docs/concept/authorised.png` (the visual spec).
- Phase 2 **Inputs to Phase 4** (the `CaseState` JSON shape + still-path layout — authoritative).

---

## Deviations from spec
_(Implementing agent: record the final HUD component structure, the exact nyx wake/overlay handling
needed, and any RPi-kiosk performance constraints that shaped the animation.)_

## Inputs to Phase 5
_(Implementing agent: note anything the HA component should surface — e.g. the web bind address, a
"kiosks taken over" status flag — and any nyx change that needs documenting in nyx's own CLAUDE.md.)_
