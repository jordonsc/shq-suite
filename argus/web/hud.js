/* =============================================================================
   ARGUS — Kiosk Command HUD (vanilla JS, no framework, no build step)

   Responsibilities:
     - generate the drawn chrome details (tick rail, crosshairs, emblems)
     - run a single cheap radar-sweep canvas
     - own the WebSocket /kiosk client with capped exponential-backoff reconnect
     - render every CaseState (or `null` idle) through one render() path
     - keep intruder cards keyed/stable by intruder.id (update in place)
     - drive the timeline ticker with a type-on effect for new lines
     - key the pulse rate/intensity to threat_level

   The render path is exported on window.ARGUS so demo.js can drive it without
   a server.  Nothing else is global.
   ============================================================================= */
(function () {
  "use strict";

  /* ---------------------------------------------------------------------------
     Small DOM helpers
     ------------------------------------------------------------------------- */
  const $ = (id) => document.getElementById(id);
  const SVGNS = "http://www.w3.org/2000/svg";
  const svgEl = (name, attrs) => {
    const el = document.createElementNS(SVGNS, name);
    for (const k in attrs) el.setAttribute(k, attrs[k]);
    return el;
  };

  const root = document.documentElement;
  const els = {
    hud: $("hud"),
    headline: $("headline"),
    emblem: $("emblem"),
    timer: $("timer"),
    clearance: $("clearance"),
    panel: $("panel"),
    camview: $("camview"),
    camImg: $("camImg"),
    camPlaceholder: $("camPlaceholder"),
    camPhText: $("camPhText"),
    camLoc: $("camLoc"),
    camAct: $("camAct"),
    camRec: $("camRec"),
    intruders: $("intruders"),
    standby: $("standby"),
    tkSummary: $("tkSummary"),
    tickerFeed: $("tickerFeed"),
    linkstat: $("linkstat"),
    lsText: $("lsText"),
  };

  /* =========================================================================
     1. DRAWN CHROME — tick rail + registration crosshairs + state emblems
     ========================================================================= */
  const VBW = 1000, VBH = 1778;

  function buildTickRail() {
    const g = $("tickRail");
    const xs = [];
    for (let x = 60; x <= 940; x += 28) xs.push(x);
    for (const x of xs) {
      const tall = (Math.round((x - 60) / 28) % 4 === 0);
      const h = tall ? 12 : 6;
      g.appendChild(svgEl("line", { x1: x, y1: 30, x2: x, y2: 30 + h }));
      g.appendChild(svgEl("line", { x1: x, y1: VBH - 30, x2: x, y2: VBH - 30 - h }));
    }
  }

  function buildCrosshairs() {
    const g = $("crosshairs");
    // mid-edge registration marks + a few interior ones (as in the concept)
    const marks = [
      // top/bottom centre marks removed — they collided with the timer and the
      // situation feed (the circle made both hard to read).
      [60, 889], [940, 889],             // left / right centre
      [260, 620], [740, 620],            // interior pair
      [260, 1160], [740, 1160],
    ];
    for (const [cx, cy] of marks) {
      const r = 9;
      g.appendChild(svgEl("line", { x1: cx - r, y1: cy, x2: cx + r, y2: cy }));
      g.appendChild(svgEl("line", { x1: cx, y1: cy - r, x2: cx, y2: cy + r }));
      g.appendChild(svgEl("circle", { cx, cy, r: r + 4 }));
    }
  }

  // Per-state emblem (top-left of the band). Rose sigil for alarm/standby, a
  // clean triangle for authorised — echoing the two concept PNGs.
  function setEmblem(state) {
    const e = els.emblem;
    while (e.firstChild) e.removeChild(e.firstChild);
    if (state === "authorised") {
      e.appendChild(svgEl("path", { d: "M32 8 L56 52 L8 52 Z" }));
      e.appendChild(svgEl("path", { d: "M32 22 L46 48 L18 48 Z", class: "fill" }));
    } else {
      // stylised blackrose: a star/asterisk burst inside a ring
      e.appendChild(svgEl("circle", { cx: 32, cy: 32, r: 22 }));
      for (let i = 0; i < 6; i++) {
        const a = (Math.PI / 3) * i;
        e.appendChild(svgEl("line", {
          x1: 32, y1: 32,
          x2: 32 + Math.cos(a) * 20, y2: 32 + Math.sin(a) * 20,
        }));
      }
      e.appendChild(svgEl("circle", { cx: 32, cy: 32, r: 5, class: "fill" }));
    }
  }

  /* =========================================================================
     2. RADAR SWEEP — single canvas, one rotating gradient wedge. Cheap.
     ========================================================================= */
  function startRadar() {
    const cv = $("radar");
    const ctx = cv.getContext("2d");
    let raf = 0, w = 0, h = 0, last = 0;
    let ang = 0;

    function resize() {
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      w = cv.clientWidth; h = cv.clientHeight;
      cv.width = Math.round(w * dpr);
      cv.height = Math.round(h * dpr);
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    }
    resize();
    window.addEventListener("resize", resize, { passive: true });

    function hue() {
      // read the live theme hue from CSS so the sweep matches the state
      return getComputedStyle(root).getPropertyValue("--hud").trim() || "#ff1f24";
    }

    function frame(t) {
      raf = requestAnimationFrame(frame);
      if (t - last < 33) return;          // cap ~30fps for the RPi
      const dt = last ? (t - last) / 1000 : 0;
      last = t;

      // sweep period from CSS var --sweep-rate (seconds)
      const period = parseFloat(getComputedStyle(root).getPropertyValue("--sweep-rate")) || 7;
      ang = (ang + (Math.PI * 2 * dt) / period) % (Math.PI * 2);

      ctx.clearRect(0, 0, w, h);
      const cx = w / 2, cy = h * 0.46;
      const R = Math.hypot(w, h);
      const wedge = 0.55; // radians

      // Trail FOLLOWS the leading line: the wedge sits BEHIND the sweep (angles
      // ang-wedge .. ang), brightest at the line and fading out into the tail —
      // so the fade trails the radial line rather than running ahead of it.
      const frac = wedge / (Math.PI * 2);
      const grad = ctx.createConicGradient
        ? ctx.createConicGradient(ang - wedge, cx, cy)
        : null;
      const c = hue();
      if (grad) {
        grad.addColorStop(0, hexA(c, 0.0));                       // tail: transparent
        grad.addColorStop(Math.min(0.999, frac), hexA(c, 0.22));  // at the line: bright
        grad.addColorStop(Math.min(1, frac + 0.0001), hexA(c, 0.0));
        ctx.fillStyle = grad;
      } else {
        // fallback: draw a triangular wedge
        ctx.fillStyle = hexA(c, 0.16);
      }
      ctx.beginPath();
      ctx.moveTo(cx, cy);
      ctx.arc(cx, cy, R, ang - wedge, ang);
      ctx.closePath();
      ctx.fill();

      // leading edge line
      ctx.strokeStyle = hexA(c, 0.5);
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      ctx.moveTo(cx, cy);
      ctx.lineTo(cx + Math.cos(ang) * R, cy + Math.sin(ang) * R);
      ctx.stroke();
    }
    raf = requestAnimationFrame(frame);

    // pause when tab hidden (kiosks rarely background, but be polite)
    document.addEventListener("visibilitychange", () => {
      if (document.hidden) { cancelAnimationFrame(raf); raf = 0; }
      else if (!raf) { last = 0; raf = requestAnimationFrame(frame); }
    });
  }

  // hex (#rrggbb) -> rgba string with alpha
  function hexA(hex, a) {
    hex = hex.replace("#", "");
    if (hex.length === 3) hex = hex.split("").map((c) => c + c).join("");
    const n = parseInt(hex, 16);
    return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${a})`;
  }

  /* =========================================================================
     3. CLOCK — local wall-clock time, shown in ALL modes.
     ========================================================================= */
  function tickTimer() {
    els.timer.textContent = new Date().toTimeString().slice(0, 8);
  }
  setInterval(tickTimer, 1000);

  /* =========================================================================
     4. STILL fetching with 404 -> drawn placeholder
        Camera stills: GET /stills/<id>.jpg
        We set src and toggle a placeholder via load/error events.
     ========================================================================= */
  function stillUrl(id) { return `stills/${encodeURIComponent(id)}.jpg`; }

  // showPh(visible:boolean) lets the caller decide HOW to hide the placeholder
  // (the camera uses [hidden], the mug uses display:none). Single owner of the
  // img.onload/onerror handlers so they can't conflict.
  function loadStill(imgEl, showPh, stillId) {
    if (!stillId) {
      imgEl.removeAttribute("src");
      imgEl.dataset.loaded = "false";
      delete imgEl.dataset.url;
      showPh(true);
      return;
    }
    const url = stillUrl(stillId);
    if (imgEl.dataset.url === url && imgEl.dataset.loaded === "true") { showPh(false); return; }
    imgEl.dataset.url = url;
    imgEl.dataset.loaded = "false";
    imgEl.onload = () => { imgEl.dataset.loaded = "true"; showPh(false); };
    imgEl.onerror = () => { imgEl.dataset.loaded = "false"; imgEl.removeAttribute("src"); showPh(true); };
    imgEl.src = url;
  }

  /* =========================================================================
     5. STATE -> THEME mapping
        status === "cleared"   -> AUTHORISED (green)
        any other active case  -> ALARM (red)
        null                   -> STANDBY (steel)
        threat_level keys pulse rate + intensity.
     ========================================================================= */
  const THREAT = {
    info:     { rate: "4.5s", alpha: 0.10, sweep: "9s", label: "INFO" },
    elevated: { rate: "2.6s", alpha: 0.20, sweep: "6s", label: "ELEVATED" },
    critical: { rate: "1.2s", alpha: 0.34, sweep: "3.4s", label: "CRITICAL" },
  };

  function applyTheme(caseState) {
    let state, headline, clearance;
    if (!caseState) {
      state = "standby";
      headline = "SYSTEM ARMED";
      clearance = "ARMED";
    } else if (caseState.status === "cleared") {
      state = "authorised";
      headline = "AUTHORISED";
      clearance = "GRANTED";
    } else {
      state = "alarm";
      headline = "ALARM MODE";
      const t = THREAT[caseState.threat_level] || THREAT.elevated;
      clearance = t.label;
    }
    root.setAttribute("data-state", state);
    els.headline.textContent = headline;
    els.clearance.textContent = clearance;
    setEmblem(state);

    // pulse + sweep keyed to threat (alarm only; calm otherwise)
    if (state === "alarm") {
      const t = THREAT[caseState.threat_level] || THREAT.elevated;
      root.style.setProperty("--pulse-rate", t.rate);
      root.style.setProperty("--pulse-alpha", String(t.alpha));
      root.style.setProperty("--sweep-rate", t.sweep);
    } else {
      root.style.removeProperty("--pulse-rate");
      root.style.removeProperty("--pulse-alpha");
      root.style.removeProperty("--sweep-rate");
    }
  }

  /* =========================================================================
     6. INTRUDER CARDS — keyed by intruder.id, updated in place
        New ids animate in (cardIn); existing cards mutate without recreate.
     ========================================================================= */
  const cardIndex = new Map(); // id -> { root, refs..., prev:{} }

  function ensureCard(intr) {
    let entry = cardIndex.get(intr.id);
    if (entry) return entry;

    const card = document.createElement("article");
    card.className = "intruder";
    card.dataset.id = intr.id;
    card.innerHTML = `
      <div class="mug">
        <img alt="" decoding="async" />
        <div class="mug-ph">NO IMG</div>
        <span class="mug-id"></span>
      </div>
      <div class="intruder-body">
        <div class="in-tag">
          <span class="in-id"></span>
          <span class="in-status"></span>
        </div>
        <div class="in-desc"></div>
        <div class="in-meta"></div>
        <div class="conf">
          <div class="conf-track"><div class="conf-fill"></div></div>
          <div class="conf-val"></div>
        </div>
      </div>`;
    entry = {
      root: card,
      img: card.querySelector(".mug img"),
      mugPh: card.querySelector(".mug-ph"),
      mugId: card.querySelector(".mug-id"),
      id: card.querySelector(".in-id"),
      status: card.querySelector(".in-status"),
      desc: card.querySelector(".in-desc"),
      meta: card.querySelector(".in-meta"),
      fill: card.querySelector(".conf-fill"),
      cval: card.querySelector(".conf-val"),
      prev: {},
    };
    cardIndex.set(intr.id, entry);
    els.intruders.appendChild(card); // animates in via CSS
    return entry;
  }

  function updateCard(intr) {
    const e = ensureCard(intr);
    const p = e.prev;

    e.id.textContent = (intr.id || "SUBJECT").toUpperCase();
    e.mugId.textContent = (intr.id || "").toUpperCase();
    e.desc.textContent = intr.descriptors || "—";
    e.status.textContent = intr.identified ? "IDENTIFIED" : "ASSESSING";
    e.root.classList.toggle("identified", !!intr.identified);

    const loc = intr.location || intr.best_camera || "";
    const act = intr.activity || "";
    e.meta.textContent = [loc, act].filter(Boolean).join(" · ");

    const conf = clamp01(intr.confidence);
    e.fill.style.right = `${(1 - conf) * 100}%`;
    e.cval.textContent = `CONF ${Math.round(conf * 100)}%`;

    // mugshot = first of best_stills
    const stillId = intr.best_stills && intr.best_stills[0] && intr.best_stills[0].id;
    loadStill(e.img, (show) => { e.mugPh.style.display = show ? "grid" : "none"; }, stillId);

    // flash on a meaningful upgrade (identified flip or new best still)
    const becameIdentified = intr.identified && !p.identified;
    const stillChanged = stillId && stillId !== p.stillId;
    if (becameIdentified || stillChanged) {
      e.root.classList.remove("flash");
      void e.root.offsetWidth; // reflow to restart anim
      e.root.classList.add("flash");
    }

    e.prev = { identified: !!intr.identified, stillId, conf };
  }

  function reconcileIntruders(list) {
    const ids = new Set();
    for (const intr of list || []) { ids.add(intr.id); updateCard(intr); }
    // remove cards whose ids are gone
    for (const [id, e] of cardIndex) {
      if (!ids.has(id)) {
        e.root.style.animation = "cardIn 0.35s reverse forwards";
        const node = e.root;
        cardIndex.delete(id);
        setTimeout(() => node.remove(), 360);
      }
    }
  }

  const clamp01 = (n) => Math.max(0, Math.min(1, typeof n === "number" ? n : 0));

  /* =========================================================================
     7. TICKER — newest-first timeline, type-on effect for genuinely new lines
     ========================================================================= */
  const seenEvents = new Set();   // dedupe key -> rendered
  let typingTimer = 0;

  function eventKey(ev) { return `${ev.at}|${ev.kind}|${ev.detail}`; }
  const MAX_FEED = 5;

  // The feed "status" beside SITUATION FEED is a SHORT controlled enum (not the
  // free-text LLM summary, which is unpredictable length and overflows). Keeps the
  // header layout fixed. Future trigger profiles branch the active case here
  // (Investigate -> "Investigating…", General -> "Assessing…").
  function feedStatus(cs) {
    if (!cs || cs.status === "cleared") return "All clear";
    return "Intrusion in progress";
  }

  function renderTicker(caseState) {
    els.tkSummary.textContent = feedStatus(caseState);

    if (!caseState || !caseState.timeline) return;
    const feed = els.tickerFeed;

    // determine which events are new (not yet seen), keep chronological order
    const events = caseState.timeline.slice();
    const fresh = events.filter((ev) => !seenEvents.has(eventKey(ev)));

    // (re)build the visible list: newest first, capped
    const ordered = events.slice().sort((a, b) => new Date(b.at) - new Date(a.at)).slice(0, MAX_FEED);

    // Build rows; only the newest fresh one gets the type-on caret
    const newestFreshKey = fresh.length ? eventKey(fresh[fresh.length - 1]) : null;
    feed.replaceChildren();
    for (const ev of ordered) {
      const li = document.createElement("li");
      const key = eventKey(ev);
      const time = fmtTime(ev.at);
      // Time + detail only — one clean line per event. The KIND label was dropped:
      // it's redundant with the detail text and ate the width on a portrait kiosk.
      li.innerHTML = `<span class="tf-time">${time}</span>` +
        `<span class="tf-text"></span>`;
      const textEl = li.querySelector(".tf-text");
      if (key === newestFreshKey) {
        li.classList.add("typing");
        typeOn(textEl, ev.detail || "", li);
      } else {
        textEl.textContent = ev.detail || "";
      }
      feed.appendChild(li);
    }
    for (const ev of events) seenEvents.add(eventKey(ev));
  }

  function typeOn(el, text, li) {
    clearTimeout(typingTimer);
    let i = 0;
    const step = () => {
      el.textContent = text.slice(0, i);
      if (i++ < text.length) typingTimer = setTimeout(step, 18);
      else if (li) li.classList.remove("typing");
    };
    step();
  }

  /* =========================================================================
     8. MAIN RENDER — single entry point for WS + demo
        Payload is a CaseState, a {type:"system"} alarm-mode frame, or null.
     ========================================================================= */
  // Mode-specific copy for the standby pane (arming/armed/disarmed). The HUD shows
  // this when the alarm is up but there is no incident yet.
  const SYS = {
    arming:     { state: "standby",    hl: "ARMING",       clr: "ARMING",  s1: "ARMING",          s2: "SECURING PERIMETER · STAND CLEAR", status: "Arming…" },
    armed:      { state: "standby",    hl: "SYSTEM ARMED", clr: "ARMED",   s1: "SYSTEM ARMED",    s2: "ALL SECTORS NOMINAL · STANDBY",    status: "All clear" },
    // green all-clear shown for a few seconds after any disarm
    authorised: { state: "authorised", hl: "AUTHORISED",   clr: "GRANTED", s1: "ACCESS GRANTED",  s2: "ALL CLEAR · STAND DOWN",           status: "All clear" },
    triggered:  { state: "standby",    hl: "ALARM",        clr: "ALARM",   s1: "ALARM",           s2: "ASSESSING…",                       status: "Intrusion in progress" },
    disarmed:   { state: "standby",    hl: "STANDBY",      clr: "OFF",     s1: "SYSTEM DISARMED", s2: "ALL SECTORS NOMINAL",              status: "All clear" },
  };

  function renderSystem(sys) {
    const m = SYS[sys.mode] || SYS.armed;
    // Theme per mode (steel standby, or emerald for authorised); clear any alarm
    // pulse/sweep overrides.
    root.setAttribute("data-state", m.state);
    setEmblem(m.state);
    root.style.removeProperty("--pulse-rate");
    root.style.removeProperty("--pulse-alpha");
    root.style.removeProperty("--sweep-rate");
    els.headline.textContent = m.hl;
    els.clearance.textContent = m.clr;
    els.standby.hidden = false;
    els.camview.style.display = "none";
    els.intruders.style.display = "none";
    reconcileIntruders([]);
    seenEvents.clear();
    els.tickerFeed.replaceChildren();
    els.tkSummary.textContent = m.status;
    const sb1 = els.standby.querySelector(".sb-1");
    const sb2 = els.standby.querySelector(".sb-2");
    if (sb1) sb1.textContent = m.s1;
    if (sb2) sb2.textContent = m.s2;
  }

  function render(data) {
    if (data && data.type === "system") { renderSystem(data); return; }
    const caseState = data;
    applyTheme(caseState);

    if (!caseState) {
      els.standby.hidden = false;
      els.camview.style.display = "none";
      els.intruders.style.display = "none";
      reconcileIntruders([]);
      seenEvents.clear();
      renderTicker(null);
      return;
    }

    els.standby.hidden = true;
    els.camview.style.display = "";
    els.intruders.style.display = "";

    // The main pane tracks the INTRUDER, not generic camera chatter — so it shows
    // the latest intruder-level activity (location/activity/best still), never an
    // unrelated camera note (e.g. the garage's parked car).
    const view = pickPrimaryView(caseState);
    els.camLoc.textContent = view ? (view.label || "CAMERA") : "—";
    els.camAct.textContent = view ? (view.activity || "") : "";
    if (els.camPhText) els.camPhText.textContent = view ? "ACQUIRING" : "NO SIGNAL";
    loadStill(els.camImg, (show) => { els.camPlaceholder.hidden = !show; }, view && view.stillId);

    els.camRec.style.visibility =
      (caseState.status === "cleared") ? "hidden" : "visible";

    reconcileIntruders(caseState.intruders);
    renderTicker(caseState);
  }

  function stillTime(i) {
    const s = i.best_stills && i.best_stills[0];
    return s ? new Date(s.captured_at || 0).getTime() : 0;
  }

  // The primary view = the most relevant INTRUDER (the latest intruder-level
  // activity), so the main pane follows the threat — not whatever camera last had
  // something to say. Ranking: currently-located first, then armed, then highest
  // confidence, then most recent best still. Only with NO intruders do we fall
  // back to a person-present location.
  function pickPrimaryView(cs) {
    const intruders = cs.intruders || [];
    if (intruders.length) {
      const i = intruders.slice().sort((a, b) => {
        const al = a.location || a.best_camera ? 1 : 0;
        const bl = b.location || b.best_camera ? 1 : 0;
        if (al !== bl) return bl - al;
        if (!!b.armed !== !!a.armed) return (b.armed ? 1 : 0) - (a.armed ? 1 : 0);
        const ac = clamp01(a.confidence), bc = clamp01(b.confidence);
        if (bc !== ac) return bc - ac;
        return stillTime(b) - stillTime(a);
      })[0];
      return {
        label: i.location || i.best_camera || "TRACKING",
        activity: i.activity || "",
        stillId: i.best_stills && i.best_stills[0] && i.best_stills[0].id,
      };
    }
    // No intruders (rare in alarm mode) — fall back to a person-present camera.
    const loc = pickPrimaryLocation(cs);
    if (!loc) return null;
    return {
      label: loc.label || loc.camera || "CAMERA",
      activity: loc.activity || "",
      stillId: pickMainStill(cs, loc),
    };
  }

  function pickPrimaryLocation(cs) {
    const locs = cs.locations || [];
    if (!locs.length) return null;
    // prefer a location with a person present + most recent last_seen
    const sorted = locs.slice().sort((a, b) => {
      if (!!b.person_present !== !!a.person_present) return (b.person_present ? 1 : 0) - (a.person_present ? 1 : 0);
      return new Date(b.last_seen || 0) - new Date(a.last_seen || 0);
    });
    return sorted[0];
  }

  function pickMainStill(cs, loc) {
    // newest best_still from an intruder at the primary location, else any.
    const intruders = cs.intruders || [];
    const atLoc = intruders.filter((i) => loc && (i.location === loc.label || i.best_camera === loc.label));
    const pool = (atLoc.length ? atLoc : intruders);
    let best = null, bestT = -1;
    for (const i of pool) {
      const s = i.best_stills && i.best_stills[0];
      if (s) {
        const t = new Date(s.captured_at || 0).getTime();
        if (t >= bestT) { bestT = t; best = s.id; }
      }
    }
    return best;
  }

  /* =========================================================================
     9. WEBSOCKET CLIENT — /kiosk with capped exponential-backoff reconnect
        Message payload = a CaseState object OR the literal JSON `null`.
     ========================================================================= */
  const MAX_BACKOFF = 15000;
  let ws = null, backoff = 500, reconnectTimer = 0, manualClose = false;

  function wsUrl() {
    const proto = location.protocol === "https:" ? "wss:" : "ws:";
    return `${proto}//${location.host}/kiosk`;
  }

  function setLink(state, text) {
    els.linkstat.dataset.link = state;
    els.lsText.textContent = text;
  }

  function connect() {
    if (manualClose) return;
    setLink("connecting", "LINK · CONNECTING");
    try {
      ws = new WebSocket(wsUrl());
    } catch (e) {
      scheduleReconnect();
      return;
    }

    ws.addEventListener("open", () => {
      backoff = 500;
      setLink("open", "LINK · ONLINE");
    });

    ws.addEventListener("message", (ev) => {
      let data;
      try { data = JSON.parse(ev.data); }
      catch (e) { console.warn("ARGUS: bad WS payload", e); return; }
      // payload is null OR a CaseState
      render(data);
    });

    ws.addEventListener("close", () => { scheduleReconnect(); });
    ws.addEventListener("error", () => { try { ws.close(); } catch (e) {} });
  }

  function scheduleReconnect() {
    if (manualClose) return;
    setLink("closed", "LINK LOST · RECONNECTING");
    clearTimeout(reconnectTimer);
    const jitter = Math.random() * 250;
    reconnectTimer = setTimeout(connect, backoff + jitter);
    backoff = Math.min(backoff * 2, MAX_BACKOFF);
  }

  /* =========================================================================
     10. Small formatters
     ========================================================================= */
  function fmtTime(iso) {
    if (!iso) return "--:--";
    const d = new Date(iso);
    if (isNaN(d)) return "--:--";
    return d.toTimeString().slice(0, 8);
  }
  function escapeHtml(s) {
    return String(s).replace(/[&<>"']/g, (c) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  }

  /* =========================================================================
     11. Boot
     ========================================================================= */
  function boot() {
    buildTickRail();
    buildCrosshairs();
    setEmblem("standby");
    startRadar();
    tickTimer();
    render(null);              // idle until first WS message

    // demo mode suppresses the live socket (demo.js drives render directly)
    const demo = new URLSearchParams(location.search).has("demo");
    if (!demo) connect();
    else setLink("open", "DEMO · OFFLINE");
  }

  // public surface for demo.js + manual poking
  window.ARGUS = { render, connect, applyTheme };

  if (document.readyState === "loading")
    document.addEventListener("DOMContentLoaded", boot);
  else boot();
})();
