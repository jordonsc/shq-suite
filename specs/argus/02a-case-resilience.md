# Phase 2a — Case Resilience (Real-Time Offsite Replication)

> Sub-spec of [Argus master plan](./00-master.md). Depends on **Phase 2** (the `CaseState`, its
> event journal, and the case-dir still files).
>
> **Status: ✅ IMPLEMENTED (2026-06-18), build warning-free.** Offsite S3
> replicator off the case dir (write-only static creds, events+stills first,
> `.uploaded` markers, watch-woken + periodic re-scan, backoff). **Live S3
> deferred** — no AWS creds this window; bucket + write-only IAM principal owed.
> See Deviations.
>
> **Build priority: immediately after Phase 2, BEFORE Phases 3–4.** Surviving evidence outranks
> intimidation theatre.
>
> **Goal:** replicate the case — every assessment, timeline event, still, and the evolving dossier —
> to an offsite, immutable store **in real time**, so the case survives destruction of `atlas` by an
> intruder.

## Context

`atlas` is on-prem and inside the threat model: an intruder can smash, unplug, or remove it during
the very incident Argus is documenting. The forensic value of the case (who, what, where, with
images) is worthless if it dies with the server. Phase 2a streams the case to AWS S3 **as it is
produced**, with credentials and bucket policy that a compromised `atlas` cannot use to tamper with
or delete prior evidence.

This is a new **output sink** parallel to voice/PagerDuty/kiosk — it subscribes to the same Phase 2
case stream. It must not block or be blocked by the others.

## Scope

**In scope:**
- An offsite replicator (`src/out/offsite.rs`) that mirrors the case dir to S3 in real time, object
  per event/still, plus a periodically-overwritten `state.json` snapshot and a final `dossier`.
- A **durable local upload queue** (the Phase 2 case dir, extended with per-object upload markers)
  with retry/backoff that survives restarts and network outages.
- S3 setup guidance: dedicated bucket, **versioning + Object Lock (compliance mode)**, SSE, a
  **write-only** (`PutObject`-only) IAM principal for atlas.
- Recovery notes: how to reconstruct a case from S3 if atlas is gone.
- Throttling/prioritisation so real-time push stays cheap and fast.

**Out of scope (M2 / note only):**
- **Out-of-band egress (LTE/cellular failover)** to survive a WAN cut — hardware; the real fix, but
  not M1. Document it as the known limitation.
- A relay/forwarder on a second host or a cloud function — M2 hardening if direct atlas→S3 is
  considered too exposed.
- A case-viewer/reconstruction UI — later (the S3 layout below is designed to make one trivial).

## Threat model & guarantees (design intent)

| Threat | Mitigation |
|--------|------------|
| Intruder destroys/steals atlas mid-case | Each event/still PUT **as produced**; a partial case is fully recoverable from S3. |
| Intruder gains atlas → its AWS creds | Creds are **`s3:PutObject` only**, scoped to the case prefix — no `Delete`/`Get`/`List`. Cannot read other cases or erase anything. |
| Attacker tries to overwrite/delete prior evidence with those creds | Bucket **versioning + Object Lock (compliance mode)** → objects are immutable for the retention period, undeletable even by the account root. |
| Network blip / brief outage | Durable local queue + retry/backoff; uploads resume. |
| Intruder cuts the WAN entirely | **Not solved in M1** — best-effort. M2: LTE failover egress. Documented limitation. |

## S3 layout (one case = one prefix)

```
s3://<bucket>/cases/<case_id>/
  manifest.json                 # case_id, started_at, premises id, schema version
  events/000001.json            # one object per CaseState mutation / timeline event (append-only)
  events/000002.json
  stills/<still_id>.jpg         # best stills (+ sampled frames per the throttle policy)
  state.json                    # latest CaseState snapshot (overwritten; versioning keeps history)
  dossier.json                  # final dossier on standdown
```
- **Object-per-event** is the key choice: no append needed, every PUT is atomic, and any subset that
  made it offsite is a coherent partial case.
- Sequence-numbered event objects (`000001…`) give deterministic ordering for reconstruction.

## Implementation

### 1. Replicator (`src/out/offsite.rs`)
- Use the official **`aws-sdk-s3`** (tokio-native). Subscribe to the Phase 2 case stream; on each
  mutation, enqueue the new event object + any new still; periodically enqueue a `state.json`
  overwrite; enqueue `dossier.json` + a `manifest` finalise on standdown.
- PUT with `Bucket`, `Key`, body, content-type. Rely on bucket-default Object Lock retention (don't
  set per-object legal holds unless needed).

### 2. Durable queue (extend the Phase 2 case dir)
- The case dir is the queue. Each event/still file gets an upload marker (e.g. a sidecar `.uploaded`
  or a small index). A background uploader walks pending items, PUTs, marks done, retries with
  exponential backoff on failure. On startup, re-scan for un-uploaded items (covers a crash/restart).
- This makes the disk journal both the local record **and** the replication queue — no separate
  buffer.

### 3. Prioritisation / throttling
- **Always, immediately:** event objects (tiny JSON) and **best stills** per intruder — these are
  the evidence.
- **Throttled/optional:** routine all-camera frames (sample, or skip) — controlled by config to keep
  bandwidth and cost sane. The structured assessments already encode "what happened"; raw frames are
  supplementary.

### 4. S3 provisioning (document in the spec + a setup note)
- Dedicated bucket in **`ap-southeast-2` (Sydney)** — *decided*; Block Public Access on.
- **Versioning enabled** + **Object Lock in compliance mode** with a **1-year retention** — *decided*.
- SSE-S3 (or SSE-KMS if you want key control).
- A dedicated IAM user/role for atlas with a policy granting **only** `s3:PutObject`
  (+ `s3:PutObjectRetention` if needed by the lock config) on `arn:aws:s3:::<bucket>/cases/*` — no
  `Delete*`, no `Get*`, no `List*`. Retrieval/admin uses a separate, locked-down principal.
- Creds via the secret store (env), never committed.

### 5. Link the evidence to the incident (optional, nice)
- Once `state.json`/dossier are offsite, the PagerDuty payload (Phase 3) can include the S3 case
  prefix (or a presigned read URL generated with the **admin** creds out-of-band, not atlas's
  write-only creds) so responders reach the surviving record.

## Config additions
```yaml
offsite:
  enabled: true
  bucket: "shq-argus-cases"
  region: "ap-southeast-2"          # away-from-premises region
  prefix: "cases"
  aws_access_key_id: "${AWS_ACCESS_KEY_ID}"
  aws_secret_access_key: "${AWS_SECRET_ACCESS_KEY}"
  upload:
    best_stills: always
    routine_frames: sampled         # always | sampled | never
    routine_sample_secs: 30
    retry_backoff_secs: [2, 5, 15, 60]
```

## Verification (definition of done)
1. Trigger → within seconds, `cases/<case_id>/events/*.json` + best stills appear in S3, and keep
   arriving as the case evolves (real-time, not on close).
2. **Kill atlas mid-case** (hard power-off) → the case in S3 is a coherent partial: every event/still
   that was produced before the kill is present and readable by the admin principal.
3. The atlas write-only creds **cannot** delete or read an object (verify the IAM policy denies it);
   Object Lock blocks deletion even with admin creds within the retention window.
4. A network outage during a case → uploads pause and **resume** when connectivity returns (durable
   queue), with no lost events.
5. Routine-frame throttling keeps bandwidth/cost within expectations; best stills + events are never
   throttled.

## References
- Phase 2 spec (the `CaseState`, event journal, case-dir/still layout — authoritative via its
  Deviations/Inputs sections).
- `aws-sdk-s3` (Rust) docs for PutObject + Object Lock retention.
- wiki `estate/shq.md` / `shq-suite-config` — record the bucket, region, IAM principal, and lock
  policy **privately** (the bucket name/region are estate detail, not public-repo content).

---

## Deviations from spec

**Status: ✅ IMPLEMENTED (2026-06-18), build warning-free; live S3 DEFERRED**
(no AWS creds this window — code + request shapes done, bucket/IAM owed).

- **Replicator** (`src/out/offsite.rs`, module `src/out/mod.rs`): `async run(cfg:
  OffsiteConfig, base: PathBuf, wake: watch::Receiver<Option<CaseState>>)`. Built
  on **`aws-sdk-s3` 1.137** (`default-features=false`, features
  `["behavior-version-latest","rustls"]` — rustls to match the rest of the crate;
  **no `aws-config`**). The client uses **explicit static credentials**
  (`Credentials::new(...)` → `Config::builder().behavior_version(latest)
  .region(...).credentials_provider(...).build()` → `Client::from_conf`) — the
  write-only key by design, **not** the ambient/instance-role chain.
- **The case dir IS the queue** (no separate buffer). Each pass walks
  `<base>/cases/`, and for every regular file that is not a `.tmp` (in-progress
  atomic write), not a `.uploaded` marker, and lacks a sibling `<file>.uploaded`,
  it reads the bytes and `PutObject`s, then atomically drops `<file>.uploaded`.
  **Prioritisation**: `events/*.json` + `stills/*.jpg` (the evidence) sort/upload
  **before** `state.json`/`dossier.json`/`manifest.json`.
- **S3 key = `<prefix>/<rel>`** where `rel` is the path relative to
  `<base>/cases/` — i.e. `<case_id>/<rest>`. With the default prefix `cases`,
  `events/000001.json` → `cases/<case_id>/events/000001.json` (the spec layout
  1:1). Content-type `image/jpeg` for `.jpg`, `application/json` for `.json`.
  Relies on **bucket-default Object Lock retention** — no per-object retention or
  legal hold is set.
- **Real-time + durable**: woken promptly by `wake.changed()` (the Phase 2 watch
  broadcast), with a `scan_interval_secs` (default 5) periodic re-scan as the
  fallback that catches stills written between events **and** does
  crash/restart recovery (the first scan re-uploads anything unmarked). A pass
  with any PUT failure escalates an inter-pass backoff through
  `retry_backoff_secs` (default `[2,5,15,60]`, capped at the last), reset on a
  clean pass. Never panics, never drops an event.
- **Wiring** (`main.rs`): `let offsite_rx = state_tx.subscribe();` is taken
  **before** `state_tx` moves into `Engine::new`; then, in daemon mode only (not
  `--once`), `if cfg.offsite.enabled { tokio::spawn(out::offsite::run(
  cfg.offsite.clone(), case::default_case_base()?, offsite_rx)); }`. If `enabled`
  but creds/bucket are empty, `run` logs a warning and the task exits cleanly —
  it never crashes the daemon (validation at spawn, not config-parse).
- **Throttle fields are near-no-ops today** (`#[allow(dead_code)]`): Phase 2 only
  persists *best stills* to disk (routine all-camera frames are not written), so
  `best_stills`/`routine_frames`/`routine_sample_secs` have no read site yet —
  kept for forward-compat when routine-frame sampling is added.
- **Owed before DoD sign-off** (none are hard blockers; recorded as owed):
  - **Bucket provisioning**: dedicated bucket in `ap-southeast-2`, Block Public
    Access on, **versioning + Object Lock compliance mode, ~1-year retention**,
    SSE-S3.
  - **Write-only IAM principal** for atlas: policy granting **only**
    `s3:PutObject` (+ `s3:PutObjectRetention` only if the lock config needs it) on
    `arn:aws:s3:::<bucket>/cases/*` — **no** `Delete*`/`Get*`/`List*`. Retrieval
    uses a separate, locked-down principal.
  - **Creds** via env (`AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`), never
    committed; bucket name/region/IAM principal recorded **privately** in
    `shq-suite-config` / wiki `estate/`.
  - **DoD 1–5** (real-time arrival, kill-atlas partial recovery, write-only creds
    can't read/delete, outage resume, throttle) — all require the live bucket.
- **Versioning**: argus bumped `0.2.0 → 0.3.0` (new feature).
- **M1 limitation reaffirmed**: a WAN cut defeats real-time push. M2 = LTE
  out-of-band egress (hardware).

## Inputs to Phase 3

- **PagerDuty → S3 link**: the case's S3 prefix is deterministic —
  `s3://<bucket>/<prefix>/<case_id>/` (`case_id` is the PagerDuty `dedup_key`),
  so Phase 3 can embed the prefix in the PD payload `custom_details` without any
  runtime S3 call. The `CaseState.case_id` is the single join key.
- **No presigned URLs from atlas**: atlas holds **write-only** creds and
  **cannot** mint read/presigned URLs. If responders need a clickable link,
  generate a presigned GET **out-of-band** with the separate admin principal (not
  in Argus). Phase 3 should embed the bare `s3://…/<case_id>/` prefix (or a
  console path), not a presigned URL.
- **`security_station_notified` timeline event**: Phase 3 emits this when it PUTs
  to PagerDuty (the `TimelineKind` is reserved for it, per Phase 2's Inputs); the
  offsite replicator will then carry that event object offsite like any other —
  no extra Phase-2a work needed.
