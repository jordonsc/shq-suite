# Phase 4 — Kiosk Takeover & the Sci-Fi HUD

> Sub-spec of [Argus master plan](./00-master.md). Depends on **Phase 2** (`CaseState` channel +
> still files) and reuses **nyx** for the takeover.
>
> **Status: ✅ IMPLEMENTED & SHAPE-VERIFIED (2026-06-18), build warning-free.**
> axum HTTP/WS server (`/alarm`, `/stills/:id`, `/kiosk` WS pushing `CaseState`)
> + the vector sci-fi HUD web app (`argus/web/`, one parametric crimson/emerald
> frame, intruder cards, ticker, radar sweep, `?demo=1` offline harness) +
> kiosk-takeover consumer (`shq_display.navigate`). **Live takeover deferred**
> (residence asleep) and **blocked on a nyx wake/kill-Chronos change**; visual
> sign-off deferred to a human via `?demo=1`. See Deviations.
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

**Status: ✅ IMPLEMENTED & SHAPE-VERIFIED (2026-06-18), build warning-free; live
kiosk takeover DEFERRED** (residence asleep — navigating real wall displays is
forbidden). Built by two agents in parallel: the axum server (Rust) and the
vector HUD web app (`argus/web/`).

- **Server** (`src/web/mod.rs`, `axum` 0.7 + `tower-http` 0.5 `fs`): `GET /alarm`
  (+ `/`) serves `argus/web/` via `ServeDir` (`web.dir`, resolved next to the
  binary); `GET /stills/:id` returns `image/jpeg` from the current case's
  `stills/<id>.jpg` (current case_id read off the watch channel, falling back to
  scanning all case dirs — robust to a just-cleared case still on screen;
  path-traversal guarded); `GET /kiosk` is a WebSocket that sends the current
  `CaseState` (or literal `null`) on connect then pushes the full serde
  `CaseState` JSON on every `watch` change. Bound on the LAN (`web.bind`, default
  `0.0.0.0:8770`) so kiosks reach `http://atlas:8770/alarm`. Locally
  smoke-tested against 127.0.0.1 (`/alarm`→200, `/kiosk`→101 + a `null` frame).
  - Note: `aws-sdk-s3` (Phase 2a) pulls a transitive `axum` 0.6; our direct
    `axum` 0.7 coexists with it and resolves our handlers — build is clean. If a
    future axum bump conflicts, pin or align versions.
- **Takeover** (`src/out/kiosks.rs`): a watch consumer that, on a case becoming
  triggered/assessing, calls `shq_display.navigate {device_id, url}` per kiosk →
  `<web.public_base>/alarm`; on `cleared`, back to each kiosk's `dashboard_url`
  (holds the HUD through `standdown` so the green AUTHORISED state shows first).
  Added `RestClient::call_service(domain, service, data)`. **Built but never
  invoked this window** — no real kiosk navigated, daemon not run. The nyx
  wake/kill-Chronos dependency is the live-takeover blocker (below).
- **Config** (`src/config.rs`): `web: Option<WebConfig {bind, public_base, dir}>`
  (absent = no HUD server) + `kiosks: Vec<KioskConfig {ha_target, dashboard_url}>`
  (empty = no takeover). `main.rs` spawns the HUD server (if `web`) and the
  takeover consumer (if `web` + non-empty `kiosks`) in **daemon mode only**, each
  taking `state_tx.subscribe()` before the sender moves into the engine.
- **HUD web app** (`argus/web/`, dependency-free HTML/CSS/SVG/Canvas/JS): ONE
  parametric frame themed by the `--hud` custom property + `data-state` —
  `null`→`standby` (steel "SYSTEM ARMED"), `cleared`→`authorised` (emerald
  `#34d058`), any other status→`alarm` (crimson `#ff1f24`); hues audited from
  `docs/concept/{alarm,authorised}.png`. Drawn chrome (segmented border, corner
  brackets, crosshairs, warning triangle, header band with a command wordmark +
  live elapsed timer + CLEARANCE/threat badge). Content panel = camera still +
  **intruder cards keyed by `intruder.id`** (slide in for new ids, mutate in place
  otherwise — no thrash) showing mugshot (`best_stills[0]` → `/stills/<id>.jpg`,
  drawn placeholder on 404), descriptors, a confidence bar, location/activity;
  plus a **type-on ticker** over `timeline`+`summary`. Motion: a single-`<canvas>`
  radar sweep (~30 fps, pauses when hidden) + a pulse border keyed to
  `threat_level` (info<elevated<critical) — GPU-cheap, honours
  `prefers-reduced-motion`, portrait-first with a landscape fallback. WS client
  auto-reconnects (capped backoff) with a CONNECTING/ONLINE/LINK-LOST indicator.
- **`?demo=1` offline harness** (`argus/web/demo.js`): suppresses the live socket
  and loops a scripted lifecycle (null → triggered/info → assessing/elevated +
  subject-1 → identified + subject-2 → critical + station-notified → standdown →
  cleared/authorised → null) through the same `render()` path, so a **human can
  eyeball the HUD offline** without the server or real stills (stills 404 →
  placeholder). `node --check` passes on `hud.js`/`demo.js`. **Visual sign-off is
  deferred to a human** (no headless browser this window).
- **Minor notes**: fonts are Saira Condensed (display) + Share Tech Mono (data)
  via a Google Fonts `<link>` with a system fallback — a kiosk needs internet for
  the webfont, else it falls back gracefully; self-hosting the fonts is a future
  hardening. The "primary camera" still is the newest best-still at the most
  active location (the WS contract carries still ids only on intruder
  `best_stills`, not standalone location stills).
- **Versioning**: argus bumped `0.4.0 → 0.5.0`.

**nyx wake / kill-Chronos dependency (Rust-side flag, live-takeover blocker).**
The takeover consumer (`src/out/kiosks.rs`) issues `shq_display.navigate`
(`{device_id, url}`) per kiosk. That service only triggers a CDP `Page.navigate`
in nyx — it does NOT:
- **wake a sleeping kiosk.** At `auto_off_time` (`idle_mode: off`) nyx blanks the
  backlight; a `navigate` reloads Chrome behind a dark panel — invisible until a
  wake restores the backlight.
- **kill a Chronos overlay.** On `idle_mode: clock` kiosks nyx spawns Chronos, a
  wlr-layer-shell **overlay** that sits *above* Chromium by protocol. `navigate`
  changes the page underneath but Chronos stays on top, so the HUD is hidden.

In nyx, only `wake` / `set_display true` / touch SIGKILLs Chronos and restores
`bright_level` (`nyx/CLAUDE.md` → "Clock screensaver"; ledger `shq-suite-0001`).
`shq_display` exposes only `navigate` today — there is **no** `shq_display.wake`
service, and `navigate` does not imply a wake.

**Resolution needed before live takeover (one of):**
1. **Preferred — nyx change:** make the nyx `navigate` command imply a wake +
   overlay-kill (SIGKILL Chronos, restore `bright_level`, ungrab) before the CDP
   `Page.navigate`. A takeover navigate is always a deliberate "show this now",
   so wake-on-navigate is the right default for the alarm path. (Document in
   `nyx/CLAUDE.md` if added.) Smallest blast radius: a one-call takeover stays
   one call.
2. **Alternative — HA-side:** add a `shq_display.wake` service (nyx already has a
   `wake` WS command) and have the consumer call `wake` then `navigate` per
   kiosk. More round-trips; needs the HA component + nyx WS `wake` exposed.

This is **NOT wired in this build** (residence asleep — no live takeover run).
The consumer is built, compiled, and locally smoke-tested (HTTP routes +
`/kiosk` WS handshake against the real `web/` app), but no real kiosk was
navigated. Until (1) or (2) lands, a live takeover would silently fail to show
on any sleeping or clock-mode kiosk.

## Inputs to Phase 5

- **The Argus daemon now hosts a LAN HTTP/WS server** (`web.bind`, default
  `0.0.0.0:8770`); `web.public_base` is what kiosks load. The Phase 5 HA
  component's **control WS** is a *separate* endpoint/port from the `/kiosk` HUD
  WS — don't conflate them. Reuse the `watch<Option<CaseState>>` subscribe
  pattern for the control surface (status push), exactly like `web`/`out`/offsite.
- **Status the HA component should surface** (all derivable from the latest
  `CaseState` on the channel): `active` (Some(case) & status != cleared),
  `case_id`, `status`, `threat_level`, `intruder count` (`intruders.len()`),
  `summary`. A **"kiosks taken over"** flag is *inferred* (case active + `kiosks`
  configured) — Phase 4 doesn't expose it explicitly; if the component wants a
  truthful flag, have the kiosk consumer report success back (a small shared
  `AtomicBool`/watch) — noted, not built.
- **nyx change to document** (`nyx/CLAUDE.md`): the wake-on-navigate /
  kill-Chronos behaviour above is a live-takeover prerequisite. Whichever
  resolution (nyx `navigate` implies wake+overlay-kill, OR a new `shq_display.wake`
  service) is chosen in Phase 5/M2, document it in `nyx/CLAUDE.md` and (if a new
  HA service) `home-assistant/CLAUDE.md`.
- **Control commands** the component will want (`acknowledge`, `standdown`,
  optionally `arm`/`disarm` proxied to the HA alarm): Argus today has no inbound
  control path — Phase 5 adds the control WS (`src/web/control.rs` or a channel
  into the engine). `standdown` maps to the engine's existing standdown path
  (currently only driven by the HA `AlarmCleared` edge); wiring a manual
  standdown means a control→engine command channel (the engine loop's `select!`
  already multiplexes `mpsc<HaEvent>` — add a control variant or a second mpsc).
- **Deploy**: the server serves `argus/web/` from `web.dir` — the deploy tool
  must rsync the `web/` assets alongside the binary, and the systemd unit's
  working dir / `web.dir` must point at them.
