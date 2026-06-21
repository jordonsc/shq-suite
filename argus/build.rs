//! Compile the Overwatch gRPC proto at build time (Phase 3 voice client).
//!
//! `proto/voice.proto` is a relative symlink to `../../overwatch/proto/voice.proto`
//! (the same single-source-of-truth trick the HA `overwatch` component uses, which
//! copies it) so the proto stays a single
//! source of truth. We build the **client** stubs only — Argus never serves the
//! VoiceService, it calls Overwatch. Matches Overwatch's own `build.rs`
//! (`tonic_build::compile_protos`) and its tonic/prost 0.11/0.12 versions so the
//! generated `voice` module is wire-compatible.
//!
//! Requires `protoc` on PATH at build time (system `protoc` on atlas / the WSL2
//! dev box, same as Overwatch — see overwatch/setup-wsl2.sh).
fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(false)
        .compile(&["proto/voice.proto"], &["proto"])?;
    Ok(())
}
