# Voice Server

A gRPC-based voice service for managing alarms and text-to-speech synthesisation using AWS Polly.

## Features

- **Alarm Management**: Enable/disable alarms that loop audio files
- **Text-to-Speech**: Synthesize and play text using AWS Polly voices
- **TTS Caching**: Filesystem caching of synthesized speech to reduce API calls and latency
- **Notification Tones**: Optional notification sounds before TTS playback
- **Configurable Voices**: Support for multiple AWS Polly voices
- **YAML Configuration**: Easy configuration of voices, alarms, and tones

## Prerequisites

- Rust 1.70+ (with Cargo)
- Protocol Buffers compiler (protoc)
- AWS credentials configured (for Polly access)
- Audio output device

### Quick Setup for WSL2

Run the automated setup script to install all dependencies:

```bash
./setup-wsl2.sh
```

This script will install:
- Rust toolchain (if not already installed)
- Protocol Buffers compiler
- ALSA and PulseAudio libraries for WSL audio
- Podman for containerized cross-compilation
- Cross tool for Raspberry Pi builds
- Configure ALSA to route audio through WSLg
- Configure Cross to use Podman instead of Docker

After running the script, restart your shell or run `source ~/.cargo/env`

### Manual Setup

For manual installation or other platforms, see the detailed setup instructions in the sections below.

## Configuration

Create a `config.yaml` file based on `config.example.yaml`:

```yaml
server_address: "0.0.0.0:50051"

# Default voice for TTS (optional, defaults to "Amy")
default_voice: "Amy"

# TTS engine (optional, defaults to "generative")
default_engine: "generative"  # Options: neural, generative, long-form, standard

# AWS configuration (optional - uses environment variables if not specified)
aws:
  region: "us-east-1"
  access_key_id: "YOUR_ACCESS_KEY_ID"
  secret_access_key: "YOUR_SECRET_ACCESS_KEY"

alarms:
  morning: "sounds/alarms/morning.mp3"

notification_tones:
  chime: "sounds/tones/chime.mp3"
```

**Note**: You can specify any supported voice in the `voice_id` parameter when calling `Verbalise`. The `default_voice` is used when no `voice_id` is provided.

**Note**: You can omit the entire `aws` section to use AWS credentials from:
- Environment variables (`AWS_REGION`, `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`)
- AWS credentials file (`~/.aws/credentials`)
- IAM role (if running on EC2/ECS)

## TTS Caching

The voice server automatically caches synthesized speech to the filesystem to reduce AWS Polly API calls and improve response times.

### How It Works

- Cache files are stored in `./cache/tts/` relative to the server's working directory
- Each cache entry is identified by a SHA-256 hash of the voice, engine, and text
- Cache files are saved as MP3 format (e.g., `a3b5c7d9e1f2...mp3`)
- On subsequent requests with the same voice, engine, and text, the cached audio is returned instantly
- Cache misses trigger AWS Polly synthesis, and the result is automatically cached for future use

### Benefits

- **Reduced latency**: Cached responses return in milliseconds instead of 1-2 seconds
- **Cost savings**: Fewer AWS Polly API calls reduce usage costs
- **Offline capability**: Previously synthesized phrases work even if AWS is unreachable

### Cache Management

The cache directory is created automatically on server startup. To clear the cache:

```bash
rm -rf ./cache/tts/*
```

## Setting Up Audio on WSL

If you're developing on WSL (Windows Subsystem for Linux), you need to install audio libraries to enable sound output through Windows.

**Quick Setup**: Run `./setup-wsl2.sh` which handles all of this automatically.

### For WSL with WSLg (Windows 11) - Manual Setup

WSLg includes PulseAudio support for audio routing to Windows. Install the required libraries:

```bash
# Update package list
sudo apt update

# Install Protocol Buffers compiler
sudo apt install -y protobuf-compiler

# Install ALSA development libraries
sudo apt install -y libasound2-dev pkg-config

# Install PulseAudio libraries
sudo apt install -y libpulse-dev pulseaudio-utils

# Install ALSA PulseAudio plugin
sudo apt install -y libasound2-plugins
```

Configure ALSA to use PulseAudio:

```bash
cat > ~/.asoundrc << 'EOF'
pcm.!default {
    type pulse
}

ctl.!default {
    type pulse
}
EOF
```

Verify PulseAudio is working:

```bash
pactl info
```

You should see output showing connection to `unix:/mnt/wslg/PulseServer` with `RDPSink` as the default sink.

### For Raspberry Pi

On Raspberry Pi OS, install the following dependencies:

```bash
sudo apt update
sudo apt install -y protobuf-compiler libasound2-dev pkg-config
```

## Building

### For Local Development (WSL/Linux x86_64)

```bash
cargo build --release
```

### For Raspberry Pi 5 (Cross-Compilation with Cross + Podman)

#### One-Time Setup

**Quick Setup**: Run `./setup-wsl2.sh` which installs everything automatically (Podman, Cross, and configuration).

**Manual Setup**:

```bash
# Install Podman
sudo apt update
sudo apt install -y podman

# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env

# Add ARM64 target
rustup target add aarch64-unknown-linux-gnu

# Install Cross
cargo install cross --git https://github.com/cross-rs/cross

# Configure Cross to use Podman
echo 'export CROSS_CONTAINER_ENGINE=podman' >> ~/.cargo/env
source ~/.cargo/env
```

#### Building

```bash
# Easy way: Use the build script
./build-rpi.sh           # Release build
./build-rpi.sh --debug   # Debug build

# Or use cross directly
cross build --release --target aarch64-unknown-linux-gnu
cross build --target aarch64-unknown-linux-gnu  # debug build
```

The compiled binary will be at: `target/aarch64-unknown-linux-gnu/release/voice-server`

**Note**: The first build will download the Cross container image (~1GB), which may take a few minutes. Subsequent builds will be much faster.

#### Deploying to Raspberry Pi

```bash
# Copy binary to Raspberry Pi
scp target/aarch64-unknown-linux-gnu/release/voice-server pi@raspberrypi.local:~/

# Copy configuration and assets
scp config.yaml pi@raspberrypi.local:~/
scp -r sounds/ pi@raspberrypi.local:~/
```

#### Alternative: Build Directly on Raspberry Pi

If you prefer not to use containers, you can build directly on the Raspberry Pi:

```bash
# On the Raspberry Pi
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
sudo apt install -y protobuf-compiler libasound2-dev pkg-config
cargo build --release
```

This is slower (~5-10 minutes) but requires no container setup.

## Running

```bash
# Use default config.yaml
cargo run

# Specify custom config file
CONFIG_PATH=/path/to/config.yaml cargo run
```

## gRPC API

### SetAlarmEnabled

Enable or disable an alarm by ID.

```protobuf
rpc SetAlarmEnabled(SetAlarmEnabledRequest) returns (SetAlarmEnabledResponse);

message SetAlarmEnabledRequest {
  string alarm_id = 1;
  bool enabled = 2;
}
```

### Verbalise

Synthesise and play text with optional notification tone, voice, and volume.

```protobuf
rpc Verbalise(VerbaliseRequest) returns (VerbaliseResponse);

message VerbaliseRequest {
  string text = 1;
  optional string notification_tone_id = 2;
  optional string voice_id = 3;
  optional float volume = 4;  // Volume level 0.0-1.0, default from config
}
```

**Volume Parameter:**
- Range: `0.0` (mute) to `1.0` (full volume)
- Values `>1.0` allowed for amplification (may cause clipping)
- If omitted, uses `default_volume` from config (defaults to 1.0)
- Applies to both notification tone and TTS audio
- Independent per request - does not affect already-playing alarms or other audio

## Supported Voices & Engines

### Voices

All AWS Polly English voices can be used by specifying the voice name in the `voice_id` parameter:

**US English**: Danielle, Gregory, Ivy, Joanna, Kendra, Kimberly, Salli, Joey, Justin, Kevin, Matthew, Ruth, Stephen, Patrick
**British English**: Amy, Emma, Brian, Arthur
**Australian English**: Nicole, Olivia, Russell
**Indian English**: Aditi, Raveena, Kajal
**Irish English**: Niamh
**New Zealand English**: Aria
**Singaporean English**: Jasmine
**South African English**: Ayanda
**Welsh English**: Geraint

To use a voice, simply specify its name in the `voice_id` field of the `Verbalise` request. If no `voice_id` is provided, the `default_voice` from the config is used (defaults to "Amy").

### Engines

Configure the TTS engine in `default_engine` (defaults to "generative"):

- **neural**: High-quality, natural-sounding speech
- **generative**: Newest, most natural-sounding speech (supports Amy, Matthew, Ruth, Stephen, and more - see [AWS docs](https://docs.aws.amazon.com/polly/latest/dg/available-voices.html))
- **long-form**: Optimised for longer content like articles and news
- **standard**: Traditional TTS engine (lower quality, but cheaper)

**Note**: Not all voices support all engines. Check the AWS Polly documentation for the latest voice-engine compatibility.

## Environment Variables

- `CONFIG_PATH`: Path to configuration file (default: `config.yaml`)

### AWS Credentials

AWS credentials can be configured in three ways (in order of precedence):

1. **Config file** (`config.yaml`):
   ```yaml
   aws:
     region: "us-east-1"
     access_key_id: "YOUR_KEY"
     secret_access_key: "YOUR_SECRET"
   ```

2. **Environment variables**:
   - `AWS_REGION`
   - `AWS_ACCESS_KEY_ID`
   - `AWS_SECRET_ACCESS_KEY`

3. **AWS credentials file** (`~/.aws/credentials`) or IAM role

## License

Proprietary
