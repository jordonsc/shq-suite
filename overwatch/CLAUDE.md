# Overwatch Voice Server

Rust gRPC server for text-to-speech (AWS Polly) and alarm playback. Runs on a dedicated RPi 5 with USB audio output.

## Source Layout

| File | Purpose |
|------|---------|
| `src/main.rs` | Entry point — loads config, starts gRPC server |
| `src/config.rs` | YAML config parsing (AWS creds, voices, sound paths) |
| `src/service.rs` | gRPC service impl — SetAlarm + Verbalise + PlayTone handlers |
| `src/tts.rs` | AWS Polly TTS — synthesises speech, caches audio |
| `src/audio.rs` | Audio playback via rodio (ALSA backend); also the alarm loop (`start_alarm_inner` plays klaxons via `repeat_infinite` until stopped) |
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

## TTS Cache

Synthesised audio is cached in `cache/` directory to avoid repeated AWS Polly calls.
