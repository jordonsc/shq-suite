//! Phase 2a — offsite case replication to S3.
//!
//! `atlas` is on-prem and inside the threat model: an intruder can smash or
//! remove it during the very incident Argus is documenting. This task mirrors the
//! Phase 2 case dir to S3 **as it is produced**, object-per-event, so any subset
//! that reached offsite is a coherent partial case.
//!
//! ## Design
//! - **The case dir IS the durable upload queue.** No separate buffer: we walk
//!   `<base>/cases/`, PUT each file, and drop an empty `<file>.uploaded` sibling
//!   marker on success. A file with a sibling marker is skipped; a `.tmp` file is
//!   an in-progress atomic write (Phase 2 writes `.tmp` then renames) and is
//!   skipped until it lands. On startup the first scan re-uploads anything lacking
//!   a marker — that's the crash/restart recovery path.
//! - **Wake + re-scan.** A `watch` receiver (the Phase 2 broadcast) wakes us
//!   promptly on each mutation; a periodic re-scan is the fallback that catches
//!   stills written between events and recovers after a restart.
//! - **Prioritisation.** Within a pass, `events/*.json` + `stills/*.jpg` (the
//!   evidence) upload before `state.json` / `dossier.json` / `manifest.json`.
//! - **Retry/backoff.** A pass with any PUT failure escalates the inter-pass sleep
//!   through `retry_backoff_secs` (capped at the last value); a clean pass resets
//!   it. Failures are logged and retried next pass — never panicked, never lost.
//! - **Write-only creds, by design.** The client uses EXPLICIT static credentials
//!   (an `s3:PutObject`-only IAM principal), NOT the ambient/instance chain — a
//!   compromised atlas cannot read or delete prior evidence. Object immutability
//!   is enforced by **bucket-default Object Lock**, so we set no per-object
//!   retention/legal-hold here.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::case::CaseState;
use crate::config::OffsiteConfig;

/// Run the offsite replicator until the process exits. Spawned from `main` only
/// when `offsite.enabled`. `base` is the case-storage base (`~/.local/share/argus`),
/// i.e. the parent of `cases/`. `wake` is a clone of the Phase 2 `CaseState`
/// broadcast receiver, used purely as a "something changed" signal.
pub async fn run(cfg: OffsiteConfig, base: PathBuf, mut wake: watch::Receiver<Option<CaseState>>) {
    let client = match build_client(&cfg) {
        Ok(c) => c,
        Err(e) => {
            warn!("offsite replication disabled: {e:#}");
            return;
        }
    };

    let cases_root = base.join("cases");
    let scan_interval = Duration::from_secs(cfg.upload.scan_interval_secs.max(1));
    info!(
        bucket = %cfg.bucket,
        region = %cfg.region,
        prefix = %cfg.prefix,
        "offsite replication active: mirroring {} → s3://{}/{}",
        cases_root.display(),
        cfg.bucket,
        cfg.prefix
    );

    // Consecutive-failure counter drives the backoff escalation.
    let mut fail_streak: usize = 0;

    loop {
        let had_failure = match run_pass(&client, &cfg, &cases_root).await {
            Ok(failed) => failed > 0,
            Err(e) => {
                // A pass-level error (e.g. cannot read the cases dir). Log + treat
                // as a failing pass so we back off; the dir may simply not exist
                // yet (no case has opened).
                debug!("offsite scan pass error: {e:#}");
                true
            }
        };

        if had_failure {
            fail_streak += 1;
            let backoff = backoff_for(&cfg.upload.retry_backoff_secs, fail_streak);
            warn!(
                "offsite pass had failures (streak {fail_streak}); backing off {}s before retry",
                backoff.as_secs()
            );
            tokio::time::sleep(backoff).await;
        } else {
            fail_streak = 0;
            // Clean pass: wait for a wake signal OR the periodic re-scan.
            tokio::select! {
                changed = wake.changed() => {
                    if changed.is_err() {
                        // Sender dropped (engine gone). Keep running on the timer
                        // so a final flush/crash-recovery still proceeds.
                        debug!("offsite wake channel closed; falling back to periodic scan only");
                        tokio::time::sleep(scan_interval).await;
                    }
                }
                _ = tokio::time::sleep(scan_interval) => {}
            }
        }
    }
}

/// Build an S3 client from EXPLICIT static write-only credentials + region — never
/// the ambient/instance credential chain. Returns an error (and the caller
/// disables replication) if `enabled` but the creds are absent.
fn build_client(cfg: &OffsiteConfig) -> Result<Client> {
    if cfg.bucket.trim().is_empty() {
        return Err(anyhow!("offsite.enabled but offsite.bucket is empty"));
    }
    if cfg.aws_access_key_id.trim().is_empty() || cfg.aws_secret_access_key.trim().is_empty() {
        return Err(anyhow!(
            "offsite.enabled but AWS credentials are empty (set AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY)"
        ));
    }

    // Static creds — `Credentials` implements `ProvideCredentials`, so it is the
    // whole credential provider. Provider name is a static label for diagnostics.
    let creds = Credentials::new(
        cfg.aws_access_key_id.clone(),
        cfg.aws_secret_access_key.clone(),
        None, // no session token (long-lived IAM user key)
        None, // no expiry
        "argus-offsite-static",
    );

    let conf = aws_sdk_s3::config::Builder::new()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new(cfg.region.clone()))
        .credentials_provider(creds)
        .build();

    Ok(Client::from_conf(conf))
}

/// One scan pass: collect pending files (prioritised), PUT each, mark uploaded.
/// Returns the number of files that FAILED to upload this pass (0 = clean).
async fn run_pass(client: &Client, cfg: &OffsiteConfig, cases_root: &Path) -> Result<usize> {
    if !cases_root.exists() {
        // No case has ever opened — nothing to do, and not a failure.
        return Ok(0);
    }

    let mut pending = collect_pending(cases_root).context("scanning case dir")?;
    if pending.is_empty() {
        return Ok(0);
    }
    // Evidence (events + stills) first, then snapshots/manifest/dossier.
    pending.sort_by_key(|p| (upload_rank(&p.rel), p.rel.clone()));

    let mut failures = 0usize;
    for item in pending {
        match upload_one(client, cfg, &item).await {
            Ok(()) => {
                if let Err(e) = mark_uploaded(&item.path) {
                    // The PUT succeeded but the marker write failed: we'll re-PUT
                    // next pass (idempotent — same key, Object Lock keeps versions).
                    warn!("offsite: uploaded {} but failed to write marker: {e:#}", item.rel);
                    failures += 1;
                }
            }
            Err(e) => {
                warn!("offsite: PUT failed for {}: {e:#}", item.rel);
                failures += 1;
            }
        }
    }
    Ok(failures)
}

/// A file awaiting upload: its absolute path and its key-relative path
/// (`<case_id>/<rest>`, i.e. relative to `cases/`).
struct Pending {
    path: PathBuf,
    rel: String,
}

/// Walk `cases/` and return every regular file that is not a `.tmp`, not an
/// `.uploaded` marker, and has no sibling `.uploaded` marker.
fn collect_pending(cases_root: &Path) -> Result<Vec<Pending>> {
    let mut out = Vec::new();
    let mut stack = vec![cases_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .with_context(|| format!("reading {}", dir.display()))?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let ft = entry.file_type()?;
            if ft.is_dir() {
                stack.push(path);
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };
            // Skip in-progress atomic writes and the markers themselves.
            if name.ends_with(".tmp") || name.ends_with(".uploaded") {
                continue;
            }
            if marker_path(&path).exists() {
                continue;
            }
            let rel = match path.strip_prefix(cases_root).ok().and_then(|r| r.to_str()) {
                Some(r) => r.replace('\\', "/"),
                None => continue,
            };
            out.push(Pending { path, rel });
        }
    }
    Ok(out)
}

/// Ordering rank: evidence first. Lower sorts earlier.
fn upload_rank(rel: &str) -> u8 {
    if rel.starts_with("events/") || rel.contains("/events/") {
        0
    } else if rel.starts_with("stills/") || rel.contains("/stills/") {
        1
    } else {
        2 // state.json / dossier.json / manifest.json
    }
}

/// PUT one file. Key = `<prefix>/<rel>` where `rel` is the path relative to
/// `cases/` (i.e. `<case_id>/<rest>`), matching the spec's S3 layout 1:1.
async fn upload_one(client: &Client, cfg: &OffsiteConfig, item: &Pending) -> Result<()> {
    let key = format!("{}/{}", cfg.prefix.trim_end_matches('/'), item.rel);
    let content_type = content_type_for(&item.rel);

    // Read the bytes ourselves (rather than `ByteStream::from_path`, which needs
    // the smithy `rt-tokio` feature) so the dep set stays minimal. Case objects
    // are small (tiny JSON, single JPEGs).
    let bytes = std::fs::read(&item.path)
        .with_context(|| format!("reading {}", item.path.display()))?;
    let body = ByteStream::from(bytes);

    client
        .put_object()
        .bucket(&cfg.bucket)
        .key(&key)
        .content_type(content_type)
        .body(body)
        .send()
        .await
        .with_context(|| format!("PutObject {key}"))?;

    info!("offsite: uploaded s3://{}/{}", cfg.bucket, key);
    Ok(())
}

fn content_type_for(rel: &str) -> &'static str {
    if rel.ends_with(".jpg") || rel.ends_with(".jpeg") {
        "image/jpeg"
    } else if rel.ends_with(".json") {
        "application/json"
    } else {
        "application/octet-stream"
    }
}

/// Sibling marker path for `<file>` → `<file>.uploaded`.
fn marker_path(file: &Path) -> PathBuf {
    let mut s = file.as_os_str().to_owned();
    s.push(".uploaded");
    PathBuf::from(s)
}

/// Atomically create the empty `.uploaded` marker (write `.tmp` then rename) so a
/// half-written marker is never mistaken for "done".
fn mark_uploaded(file: &Path) -> Result<()> {
    let marker = marker_path(file);
    let mut tmp = marker.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, b"").with_context(|| format!("writing marker tmp {}", tmp.display()))?;
    std::fs::rename(&tmp, &marker)
        .with_context(|| format!("renaming marker {}", marker.display()))?;
    Ok(())
}

/// Backoff duration for the Nth consecutive failing pass (1-based), capped at the
/// last schedule value. An empty schedule falls back to 5s.
fn backoff_for(schedule: &[u64], fail_streak: usize) -> Duration {
    if schedule.is_empty() {
        return Duration::from_secs(5);
    }
    let idx = fail_streak.saturating_sub(1).min(schedule.len() - 1);
    Duration::from_secs(schedule[idx])
}
