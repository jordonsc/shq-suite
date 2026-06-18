//! Build/version identity, logged at startup and reported downstream in later phases.

/// Semantic version sourced from `Cargo.toml`. Bump on every deployed change
/// (see the project versioning policy in the root `CLAUDE.md`).
pub const ARGUS_VERSION: &str = env!("CARGO_PKG_VERSION");
