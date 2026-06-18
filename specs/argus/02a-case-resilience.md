# Phase 2a — Case Resilience (Real-Time Offsite Replication)

> Sub-spec of [Argus master plan](./00-master.md). Depends on **Phase 2** (the `CaseState`, its
> event journal, and the case-dir still files).
>
> **Status: 📝 NOT STARTED.**
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
_(Implementing agent: record the final S3 layout, the queue/marker mechanism, the exact IAM policy +
Object Lock retention used, throttle numbers, and how the kill-atlas-mid-case test went.)_

## Inputs to Phase 3
_(Implementing agent: if PagerDuty payloads should link the S3 case, document the URL/prefix scheme
and how presigned read URLs are minted out-of-band — atlas's write-only creds cannot generate read
URLs.)_
