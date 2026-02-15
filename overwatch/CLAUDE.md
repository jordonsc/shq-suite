# Overwatch Voice Server

Rust gRPC server for text-to-speech (AWS Polly) and alarm playback. Runs on a dedicated RPi 5 with USB audio output.

## Source Layout

| File | Purpose |
|------|---------|
| `src/main.rs` | Entry point — loads config, starts gRPC server |
| `src/config.rs` | YAML config parsing (AWS creds, voices, sound paths) |
| `src/voice.rs` | gRPC service impl — SetAlarm + Verbalise handlers |
| `src/tts.rs` | AWS Polly TTS — synthesises speech, caches audio |
| `src/audio.rs` | Audio playback via rodio (ALSA backend) |
| `src/alarm.rs` | Alarm loop — plays klaxon sounds in a loop until stopped |
| `proto/voice.proto` | gRPC service definition (source of truth) |
| `build.rs` | Compiles proto at build time via tonic-build |

## gRPC API (port 50051)

```protobuf
service VoiceService {
  rpc SetAlarm(SetAlarmRequest) returns (SetAlarmResponse);
  rpc Verbalise(VerbaliseRequest) returns (VerbaliseResponse);
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
