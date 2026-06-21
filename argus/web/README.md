# Argus Kiosk HUD (`argus/web/`)

The sci-fi command-centre HUD the wall kiosks display full-screen during an
alarm (Phase 4). A **dependency-free static web app** — no framework, no build
step, no npm. Argus's `axum` server serves this directory at `/alarm`, the
camera stills at `/stills/<id>.jpg`, and pushes `CaseState` JSON over the
`/kiosk` WebSocket.

The chrome is **drawn** (SVG + CSS + a single canvas). The only raster content
is the live camera stills/mugshots dropped into the drawn frame. The two concept
PNGs (`docs/concept/alarm.png`, `authorised.png`) are the *same parametric frame*
in two hues — so this is **one component** re-themed, not two layouts.

## Files

| File | Role |
|------|------|
| `index.html` | The HUD markup — header band, headline, content panel (camera + intruder cards + ticker), the drawn-chrome `<svg>` and radar `<canvas>` scaffolding. Loads `demo.js` only on `?demo=1`. |
| `hud.css` | All styling + the **parametric theming** (the `--hud` custom property), the drawn frame, threat-keyed pulse, slide-in/type-on motion. Portrait-first; relaxes to a two-column panel in landscape. |
| `hud.js` | Chrome generation (tick rail, crosshairs, emblems), the radar canvas, the single `render(caseState)` path, the keyed intruder cards, the type-on ticker, and the `/kiosk` **WebSocket client with reconnect**. Exposes `window.ARGUS.render` for the demo. |
| `demo.js` | Offline harness (loaded on `?demo=1`). Plays a scripted `CaseState` lifecycle through `window.ARGUS.render` — no server needed. Loops every 22 s. |

## Theming — one frame, three states

Everything keys off `<html data-state="...">` and the `--hud` custom property
(plus `--hud-dim`/`--hud-glow`/`--hud-ink`/`--hud-rgb`). `hud.js` sets the
attribute from the `CaseState.status`:

| `status` | `data-state` | Hue (`--hud`) | Headline |
|----------|--------------|---------------|----------|
| *(none — payload is `null`)* | `standby` | steel-cyan `#4a7d96` | `SYSTEM ARMED` |
| `cleared` | `authorised` | emerald `#34d058` | `AUTHORISED` |
| `triggered` / `assessing` / `standdown` | `alarm` | crimson `#ff1f24` | `ALARM MODE` |

To re-skin, change the tokens in the `:root` / `[data-state=...]` blocks at the
top of `hud.css`. No JS change needed.

The **pulse border + radar sweep** are keyed to `threat_level`
(`info` < `elevated` < `critical`) via `--pulse-rate` / `--pulse-alpha` /
`--sweep-rate`, set by `applyTheme()` — `critical` pulses fastest/brightest.

## Data contract

Connects to `ws(s)://<window.location.host>/kiosk` (derived from
`window.location`, so it works wherever served). Each message is **either the
literal JSON `null`** (no active case → idle STANDBY state) **or** a `CaseState`
object:

```json
{
  "case_id": "case-…", "started_at": "ISO8601",
  "status": "triggered | assessing | standdown | cleared",
  "summary": "…", "threat_level": "info | elevated | critical",
  "locations": [ { "camera": "…", "label": "Garage", "activity": "…",
                   "person_present": false, "last_seen": "ISO8601" } ],
  "intruders": [ { "id": "subject-1", "descriptors": "…", "confidence": 0.82,
                   "location": "Garage", "activity": "…",
                   "best_stills": [ { "id": "still-0001", "camera": "…",
                                      "captured_at": "ISO8601" } ],
                   "identified": true, "dossier": "…", "best_camera": "Garage" } ],
  "timeline": [ { "at": "ISO8601", "kind": "…", "detail": "…" } ],
  "updated_at": "ISO8601", "schema_version": 1
}
```

- **Stills:** fetched from `GET /stills/<id>.jpg` where `<id>` is
  `intruders[].best_stills[].id`. The first `best_stills` entry is an intruder
  card's mugshot; the newest still at the primary location is the main camera
  view. A 404 (or missing id) falls back to a **drawn placeholder** frame — so
  the demo renders correctly with no real images.
- **Reconnect:** capped exponential backoff (500 ms → 15 s, with jitter). A
  subtle top-right indicator shows `CONNECTING` / `ONLINE` / `LINK LOST ·
  RECONNECTING`.

## Intruder cards & ticker

- Cards are **keyed by `intruder.id`** in a `Map`. New ids slide in
  (CSS `cardIn`); existing cards are mutated **in place** (no recreate/thrash),
  and only flash on a meaningful upgrade (became `identified`, or a new best
  still). Cards whose id disappears animate out and are removed.
- The **ticker** renders `timeline` newest-first (capped) plus the `summary`.
  Genuinely new events (tracked by an `at|kind|detail` key) get a **type-on**
  effect with a blinking caret; previously-seen lines render instantly.

## Performance (RPi 5 kiosks)

Animation is restricted to `transform` / `opacity` / `box-shadow` and one
canvas capped at ~30 fps. No layout-thrashing keyframes, no large blur filters.
Honours `prefers-reduced-motion`. The radar pauses when the tab is hidden.

## Demo harness — `?demo=1`

Open `index.html?demo=1` (or serve it: `python3 -m http.server` in this dir,
then `http://localhost:8000/?demo=1`). The live socket is suppressed and
`demo.js` plays this scripted lifecycle on a 22 s loop:

| t | Frame | What you see |
|---|-------|--------------|
| 0 s | `null` | **STANDBY** — steel chrome, rotating armed ring, "SYSTEM ARMED" |
| 2 s | `triggered` / `info` | flips to **ALARM** (red), camera "ACQUIRING", slow pulse |
| 4 s | `assessing` / `elevated` | `subject-1` card slides in (ASSESSING), confidence bar fills |
| 6.5 s | `assessing` / `elevated` | `subject-1` → **IDENTIFIED** (flash + dossier), `subject-2` slides in |
| 9 s | `assessing` / `critical` | threat → **CRITICAL** (fast/bright pulse), "security station notified" |
| 12 s | `standdown` / `elevated` | resident stand-down line types into the ticker |
| 14 s | `cleared` | flips to **AUTHORISED** (green), "AUTHORISED", calm |
| 18 s | `null` | back to STANDBY, then loops |

Stills 404 in the static demo, so every frame exercises the placeholder path.

## Fonts

A single Google Fonts `<link>` loads **Saira Condensed** (industrial display)
and **Share Tech Mono** (data/mono). Both fall back to system stacks
(`Arial Narrow` / monospace) — the HUD renders fully if the fonts fail to load.
