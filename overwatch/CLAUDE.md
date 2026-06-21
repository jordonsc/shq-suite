# Overwatch Voice Server

Rust gRPC server for text-to-speech (AWS Polly) and alarm playback. Runs on a dedicated RPi 5 with USB audio output.

## Source Layout

| File | Purpose |
|------|---------|
| `src/main.rs` | Entry point — loads config, starts gRPC server |
| `src/config.rs` | YAML config parsing (AWS creds, voices, sound paths) |
| `src/service.rs` | gRPC service impl — SetAlarm + Verbalise + PlayTone handlers |
| `src/tts.rs` | AWS Polly TTS — synthesises speech, caches audio |
| `src/audio.rs` | Audio playback via rodio (ALSA backend); also the alarm loop (`start_alarm_inner` plays klaxons via `repeat_infinite` until stopped). `play_bytes(.., blocking)` selects `play_bytes_inner` (detach, fire-and-forget) vs `play_bytes_blocking_inner` (bounded `sink.empty()` poll, blocks the audio thread until the clip finishes) — the latter drives Verbalise's `await_playback`. `duck_alarms(factor)`/`restore_alarms()` scale EVERY active alarm sink by a factor (e.g. 0.5 = half) / restore it to its stored canonical `AlarmState.volume` (the duck never mutates the stored value) — drives Verbalise's `duck_alarm_factor` (relative factor since 0.4.1). FIFO on the single audio thread guarantees a Duck sent before the blocking play applies first and the Restore after. **Self-healing output (0.5.0):** the `OutputStream` is held as an `Option` and reacquired on demand instead of opened once for the process life. `ensure_stream()` runs before every play/alarm — `probe_alive()` plays ~20 ms of silence to a throwaway sink and checks it drains (the only reliable signal; rodio/cpal silently swallow USB-device removal, so `Sink::try_new`/decode keep "succeeding" into a dead stream). On a failed probe it drops & reopens `OutputStream::try_default()` (reacquires the re-enumerated card 2 via the ALSA `default`) and `resume_alarms()` re-arms any sounding klaxon on the new handle. The 3 s idle tick also heals while an alarm is active, so a klaxon resumes within seconds of the amp/DAC powering back on. The audio thread no longer dies if no device exists at startup. |
| `proto/voice.proto` | gRPC service definition (source of truth) |
| `build.rs` | Compiles proto at build time via tonic-build |

## gRPC API (port 50051)

```protobuf
service VoiceService {
  rpc SetAlarm(SetAlarmRequest) returns (SetAlarmResponse);
  rpc Verbalise(VerbaliseRequest) returns (VerbaliseResponse);
  rpc PlayTone(PlayToneRequest) returns (PlayToneResponse);
}
```

### SetAlarm
- `alarm_id`: string key from config (e.g. "security", "fire", "comical")
- `enabled`: start/stop the alarm loop
- `volume`: optional 0.0-1.0

### Verbalise
- `text`: text to synthesise and speak
- `notification_tone_id`: optional tone to play first (e.g. "notify", "warn", "error")
- `voice_id`: optional AWS Polly voice (default "Amy")
- `volume`: optional 0.0-1.0
- `await_playback`: optional bool (default `false`). When `false` (default — unchanged HA TTS behaviour), the RPC returns after **synthesis** and the sink is detached (`play_bytes_inner`), so concurrent calls mix/overlap. When `true`, playback uses `sink.sleep_until_end()` (`play_bytes_blocking_inner`) so the RPC blocks until the clip **finishes playing** — used by Argus to serialise speech without timing guesses. Tradeoff: a blocking clip holds the single audio thread for the clip length, deferring other audio commands (e.g. StopAlarm); the looping klaxon sink keeps sounding meanwhile.
- `duck_alarm_factor`: optional float (added 0.4.1; was `duck_alarm_volume`, an absolute target, in 0.4.0). When `0<f<1`, EVERY active alarm (klaxon) sink is scaled by this factor (e.g. 0.5 = half its canonical volume) just before playback and restored to its canonical volume just after, so a spoken intimidation line stays intelligible over a sounding klaxon. Absent/0 = no ducking. No-op when no alarm is active. The restore fires regardless of playback outcome (a failed clip never leaves the klaxon stuck low). **Only meaningful with `await_playback=true`** (Argus, the only caller, always sets both): with fire-and-forget the sink is detached and the restore would un-duck before the clip ends.

### PlayTone
- `tone_id`: string key from the `notification_tones` config map (same files Verbalise plays as a prefix) — e.g. "notify", "warn", "error"
- `volume`: optional 0.0-1.0
- Plays a single tone and returns immediately (fire-and-forget; the rodio sink is detached). No TTS, no AWS call.

## Configuration (`config.yaml`)

```yaml
server_address: "0.0.0.0:50051"
aws:
  region: "us-west-2"
  access_key_id: "..."
  secret_access_key: "..."
default_voice: "Amy"
default_volume: 0.75
default_engine: "generative"    # neural, generative, long-form, standard
alarms:
  security: "sounds/alarms/klaxon-1.mp3"
notification_tones:
  notify: "sounds/tones/notification-1.mp3"
```

## Sounds

- `sounds/alarms/` — Klaxon MP3s for alarm loops
- `sounds/tones/` — Short notification chimes played before TTS

## Building

```bash
./setup-wsl2.sh                # One-time WSL2 dev setup (Rust, protoc, ALSA libs, cross)
cargo build --release          # Local
./build-rpi.sh                 # ARM64 for RPi (uses cross + Podman)
```

Output: `build/overwatch`

Requires `protoc` for proto compilation at build time. The `Cross.toml` installs protoc inside the container for cross-compilation.

## Audio

Uses ALSA with dmix for concurrent playback. The deploy tool installs `/etc/asound.conf` routing to USB DAC (card 2).

**USB DAC resilience (0.5.0):** the output stream auto-reconnects when the amp/USB DAC is power-cycled. Pre-0.5.0, `OutputStream::try_default()` was opened once at startup and cached for the process life; unplugging the DAC (e.g. powering the amp off) left a dead cached stream while `Sink`/decode kept succeeding silently, so playback was lost until a manual `systemctl --user restart overwatch`. Now `ensure_stream()` actively probes liveness before every play and reopens the device when needed — see the `src/audio.rs` row. The ALSA card index can also shift on re-enumeration; the `default` PCM is pinned to the USB DAC by name in `/etc/asound.conf`, so reopening reacquires it regardless of index.

## TTS Cache

Synthesised audio is cached in `cache/` directory to avoid repeated AWS Polly calls.
