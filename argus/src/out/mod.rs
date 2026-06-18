//! Output sinks that consume the Phase 2 case stream + case dir.
//!
//! These render/replicate the [`crate::case::CaseState`] — none of them call the
//! LLM or HA. The first is **`offsite`** (Phase 2a): a real-time S3 mirror of the
//! on-disk case dir so a case survives destruction of `atlas` mid-incident.

pub mod offsite;
