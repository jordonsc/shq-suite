/* =============================================================================
   ARGUS HUD — offline demo harness  (loaded only on ?demo=1)

   Feeds a scripted sequence of CaseState messages into the SAME render path the
   WebSocket uses (window.ARGUS.render), so the visuals can be eyeballed without
   the Rust server. Stills reference ids that 404 in this static context, so the
   drawn placeholders exercise their fallback path automatically.

   The sequence walks the full lifecycle:
     0.0s  null                         -> STANDBY (steel, "SYSTEM ARMED")
     2.0s  triggered  / info            -> ALARM (red), camera acquiring
     4.0s  assessing  / elevated        -> subject-1 card slides in (ASSESSING)
     6.5s  assessing  / elevated        -> subject-1 IDENTIFIED + dossier + 2nd subject
     9.0s  assessing  / critical        -> threat escalates (fast pulse), station notified
    12.0s  standdown  / elevated        -> resident standdown
    14.0s  cleared                      -> AUTHORISED (green, "AUTHORISED")
    18.0s  null                         -> back to STANDBY (loops)
   ============================================================================= */
(function () {
  "use strict";

  const T0 = "2026-06-18T22:14:00Z";
  const iso = (offsetSec) => new Date(Date.parse(T0) + offsetSec * 1000).toISOString();

  const cam = "camera.garage_side";
  const camFront = "camera.front_drive";

  // --- timeline accretes across frames (server sends the whole array each time)
  const timeline = [];
  const ev = (atSec, kind, detail) => { timeline.push({ at: iso(atSec), kind, detail }); return timeline.slice(); };

  // Build each CaseState frame as the server would emit it.
  function frameTriggered() {
    ev(0, "case_opened", "Motion + unexpected presence at Garage (side door).");
    return {
      case_id: "case-20260618T221400Z",
      started_at: iso(0),
      status: "triggered",
      summary: "Unexpected motion at the garage side door. Assessing.",
      threat_level: "benign",
      // priority-ranked PRIMARY image — populated even before any subject of
      // concern, so the main pane is never blank when there's activity.
      main_still: { id: "still-0001", camera: cam, captured_at: iso(2) },
      main_still_category: "any",
      locations: [
        { camera: cam, label: "Garage", activity: "movement at the side door",
          person_present: true, last_seen: iso(2) },
      ],
      persons: [],
      timeline: timeline.slice(),
      updated_at: iso(2), schema_version: 1,
    };
  }

  function frameAssessing() {
    ev(4, "intruder_detected", "One subject at Garage — assessing identity.");
    return {
      case_id: "case-20260618T221400Z",
      started_at: iso(0),
      status: "assessing",
      summary: "One subject at the garage door, trying the handle.",
      threat_level: "threat_present",
      main_still: { id: "still-0001", camera: cam, captured_at: iso(4) },
      main_still_category: "intruder",
      locations: [
        { camera: cam, label: "Garage", activity: "trying the side door handle",
          person_present: true, last_seen: iso(4) },
      ],
      persons: [
        { id: "subject-1", descriptors: "male, tall, dark hooded jacket, light trousers",
          resident_confidence: 0.05, guest_confidence: 0.05, intruder_confidence: 0.46, id_score: 0.8, injury: null, in_duress: 0.0, location: "Garage", activity: "trying the door handle",
          best_stills: [{ id: "still-0001", camera: cam, captured_at: iso(4) }],
          identified: false, dossier: "", best_camera: "Garage" },
      ],
      timeline: timeline.slice(),
      updated_at: iso(4), schema_version: 1,
    };
  }

  function frameIdentified() {
    ev(6, "intruder_identified", "Subject-1 identified with high confidence.");
    ev(6, "best_still_upgraded", "Better facial still captured for subject-1.");
    ev(6, "intruder_detected", "Second subject detected at the front drive.");
    return {
      case_id: "case-20260618T221400Z",
      started_at: iso(0),
      status: "assessing",
      summary: "Two subjects: one forcing the garage door, a lookout on the drive.",
      threat_level: "threat_present",
      locations: [
        { camera: cam, label: "Garage", activity: "forcing the side door",
          person_present: true, last_seen: iso(6) },
        { camera: camFront, label: "Front Drive", activity: "keeping watch",
          person_present: true, last_seen: iso(6) },
      ],
      persons: [
        { id: "subject-1", descriptors: "male, tall, dark hooded jacket, light trousers, gloved",
          resident_confidence: 0.05, guest_confidence: 0.05, intruder_confidence: 0.88, id_score: 0.8, injury: null, in_duress: 0.0, location: "Garage", activity: "forcing the side door",
          best_stills: [{ id: "still-0007", camera: cam, captured_at: iso(6) }],
          identified: true,
          dossier: "Adult male ~1.85 m, athletic build. Dark waxed jacket, hood up, light cargo trousers, work gloves. Carrying a pry tool in the right hand. No visible insignia.",
          best_camera: "Garage" },
        { id: "subject-2", descriptors: "shorter figure, hi-vis over dark clothing, cap",
          resident_confidence: 0.05, guest_confidence: 0.05, intruder_confidence: 0.34, id_score: 0.8, injury: null, in_duress: 0.0, location: "Front Drive", activity: "keeping watch",
          best_stills: [{ id: "still-0009", camera: camFront, captured_at: iso(6) }],
          identified: false, dossier: "", best_camera: "Front Drive" },
      ],
      timeline: timeline.slice(),
      updated_at: iso(6), schema_version: 1,
    };
  }

  function frameCritical() {
    ev(9, "threat_level_changed", "Threat raised to CRITICAL — forced entry in progress.");
    ev(9, "security_station_notified", "Security station paged (PagerDuty).");
    return {
      case_id: "case-20260618T221400Z",
      started_at: iso(0),
      status: "assessing",
      summary: "Forced entry in progress at the garage. Security station notified.",
      threat_level: "life_threatening",
      main_still: { id: "still-0014", camera: cam, captured_at: iso(9) },
      main_still_category: "life_threatening",
      threats: ["pry tool in subject-1's hand", "side door forced"],
      malicious_activity: ["forcing the side door", "breaking into garage"],
      locations: [
        { camera: cam, label: "Garage", activity: "door breached, entering",
          person_present: true, last_seen: iso(9) },
        { camera: camFront, label: "Front Drive", activity: "keeping watch",
          person_present: true, last_seen: iso(9) },
      ],
      persons: [
        { id: "subject-1", descriptors: "male, tall, dark hooded jacket, light trousers, gloved",
          resident_confidence: 0.05, guest_confidence: 0.05, intruder_confidence: 0.94, id_score: 0.8,
          armed: true, weapon: "pry tool", injury: null, in_duress: 0.0, location: "Garage", activity: "entering the garage",
          best_stills: [{ id: "still-0014", camera: cam, captured_at: iso(9) }],
          identified: true,
          dossier: "Adult male ~1.85 m, athletic build. Dark waxed jacket, hood up, light cargo trousers, work gloves. Pry tool used to defeat the side door. Now inside the garage.",
          best_camera: "Garage" },
        { id: "subject-2", descriptors: "shorter figure, hi-vis over dark clothing, cap, radio",
          resident_confidence: 0.05, guest_confidence: 0.05, intruder_confidence: 0.61, id_score: 0.8, injury: null, in_duress: 0.0, location: "Front Drive", activity: "keeping watch, on a radio",
          best_stills: [{ id: "still-0016", camera: camFront, captured_at: iso(9) }],
          identified: true, dossier: "Second male, ~1.70 m. Hi-vis vest over dark clothing, baseball cap. Holding a handheld radio.",
          best_camera: "Front Drive" },
      ],
      timeline: timeline.slice(),
      updated_at: iso(9), schema_version: 1,
    };
  }

  function frameStanddown() {
    ev(12, "standdown", "Resident stand-down received — disarming.");
    const f = frameCritical();
    f.status = "standdown";
    f.threat_level = "threat_present";
    f.summary = "Resident stand-down received. Stand by for all-clear.";
    f.timeline = timeline.slice();
    f.updated_at = iso(12);
    return f;
  }

  function frameCleared() {
    ev(14, "cleared", "Case cleared. Premises authorised.");
    return {
      case_id: "case-20260618T221400Z",
      started_at: iso(0),
      status: "cleared",
      summary: "All clear. Premises authorised by resident.",
      threat_level: "benign",
      locations: [
        { camera: cam, label: "Garage", activity: "clear", person_present: false, last_seen: iso(14) },
      ],
      persons: [],
      timeline: timeline.slice(),
      updated_at: iso(14), schema_version: 1,
    };
  }

  // Schedule (ms offset -> frame). null returns to standby.
  const script = [
    [0,     () => null],
    [2000,  frameTriggered],
    [4000,  frameAssessing],
    [6500,  frameIdentified],
    [9000,  frameCritical],
    [12000, frameStanddown],
    [14000, frameCleared],
    [18000, () => null],   // loop reset
  ];

  function play() {
    if (!window.ARGUS || !window.ARGUS.render) {
      // hud.js not ready yet — retry shortly
      setTimeout(play, 50);
      return;
    }
    timeline.length = 0;
    for (const [at, build] of script) {
      setTimeout(() => {
        if (at === 0) timeline.length = 0; // reset accretion at loop start
        window.ARGUS.render(build());
      }, at);
    }
    // loop the whole sequence
    setTimeout(play, 22000);
  }

  console.log("ARGUS demo harness: playing scripted CaseState sequence (loops every 22s).");
  play();
})();
