use rodio::buffer::SamplesBuffer;
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug)]
pub enum AudioCommand {
    PlayFile {
        path: PathBuf,
        volume: f32,
        response: oneshot::Sender<anyhow::Result<()>>,
    },
    PlayBytes {
        data: Vec<u8>,
        volume: f32,
        /// If true, block the audio thread until the clip finishes playing
        /// (bounded poll on `sink.empty()`) so the oneshot — and thus the RPC —
        /// only returns after playback. Tradeoff: a blocking clip defers other
        /// audio commands (e.g. StopAlarm) on the single audio thread by up to the
        /// clip length; the looping klaxon sink keeps sounding meanwhile.
        blocking: bool,
        response: oneshot::Sender<anyhow::Result<()>>,
    },
    StartAlarm {
        alarm_id: String,
        path: PathBuf,
        volume: f32,
        response: oneshot::Sender<anyhow::Result<()>>,
    },
    StopAlarm {
        alarm_id: String,
        response: oneshot::Sender<bool>,
    },
    /// Temporarily scale EVERY active alarm sink by `factor` (e.g. 0.5 = half) so a
    /// spoken line stays intelligible over a klaxon. Does NOT touch the stored
    /// canonical `AlarmState.volume` — `RestoreAlarms` puts each sink back to it.
    DuckAlarms {
        factor: f32,
        response: oneshot::Sender<()>,
    },
    /// Restore every active alarm sink to its stored canonical `AlarmState.volume`.
    RestoreAlarms {
        response: oneshot::Sender<()>,
    },
}

pub struct AudioManager {
    command_tx: mpsc::UnboundedSender<AudioCommand>,
}

struct AudioManagerInner {
    /// The active rodio output stream and its handle. `None` when no working audio
    /// device is currently held — e.g. the USB DAC was unplugged/powered off, or
    /// none was available at startup. `ensure_stream()` (re)opens it on demand, so
    /// the daemon self-heals when the amp/DAC is powered back on. They are always
    /// set/cleared together; `stream` must be kept alive for `stream_handle` to play.
    stream: Option<OutputStream>,
    stream_handle: Option<OutputStreamHandle>,
    active_alarms: HashMap<String, AlarmState>,
}

struct AlarmState {
    sink: Sink,
    path: PathBuf,
    volume: f32,
    started_at: Instant,
}

impl AudioManager {
    pub fn new() -> anyhow::Result<Self> {
        let (command_tx, command_rx) = mpsc::unbounded_channel();

        // Spawn dedicated audio thread. The thread stays alive even if no audio
        // device is available at startup — it heals on demand via ensure_stream().
        std::thread::spawn(move || {
            let mut inner = AudioManagerInner::new();
            inner.run(command_rx);
        });

        Ok(Self { command_tx })
    }

    pub async fn play_file(&self, path: PathBuf, volume: f32) -> anyhow::Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(AudioCommand::PlayFile {
                path,
                volume,
                response: response_tx,
            })
            .map_err(|_| anyhow::anyhow!("Audio thread died"))?;
        response_rx.await?
    }

    /// Play raw decoded audio bytes. When `blocking` is true the call resolves only
    /// after playback finishes (the audio thread holds on a bounded `sink.empty()`
    /// poll); when false it detaches the sink and returns immediately (fire-and-forget).
    pub async fn play_bytes(&self, data: Vec<u8>, volume: f32, blocking: bool) -> anyhow::Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(AudioCommand::PlayBytes {
                data,
                volume,
                blocking,
                response: response_tx,
            })
            .map_err(|_| anyhow::anyhow!("Audio thread died"))?;
        response_rx.await?
    }

    pub async fn start_alarm(&self, alarm_id: String, path: PathBuf, volume: f32) -> anyhow::Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(AudioCommand::StartAlarm {
                alarm_id,
                path,
                volume,
                response: response_tx,
            })
            .map_err(|_| anyhow::anyhow!("Audio thread died"))?;
        response_rx.await?
    }

    pub async fn stop_alarm(&self, alarm_id: String) -> bool {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(AudioCommand::StopAlarm {
                alarm_id,
                response: response_tx,
            })
            .ok();
        response_rx.await.unwrap_or(false)
    }

    /// Scale ALL active alarm sinks by `factor` (e.g. 0.5 = half; transient — the
    /// stored canonical volume is untouched). No-op if no alarm is active. Resolves
    /// once the audio thread has applied it, so a caller can sequence Duck → play →
    /// Restore.
    pub async fn duck_alarms(&self, factor: f32) {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(AudioCommand::DuckAlarms {
                factor,
                response: response_tx,
            })
            .ok();
        let _ = response_rx.await;
    }

    /// Restore ALL active alarm sinks to their stored canonical volume. No-op if
    /// no alarm is active.
    pub async fn restore_alarms(&self) {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(AudioCommand::RestoreAlarms {
                response: response_tx,
            })
            .ok();
        let _ = response_rx.await;
    }
}

/// Build a looping (infinite) alarm sink on the given output handle. Shared by the
/// initial StartAlarm and by alarm resumption after the stream is reopened.
fn make_alarm_sink(handle: &OutputStreamHandle, path: &Path, volume: f32) -> anyhow::Result<Sink> {
    let file = File::open(path)?;
    let source = Decoder::new(BufReader::new(file))?.repeat_infinite();
    let sink = Sink::try_new(handle)?;
    sink.set_volume(volume);
    sink.append(source);
    Ok(sink)
}

impl AudioManagerInner {
    fn new() -> Self {
        let mut inner = Self {
            stream: None,
            stream_handle: None,
            active_alarms: HashMap::new(),
        };
        // Try to open the output now, but don't fail the thread if the device is
        // absent (e.g. amp powered off at boot) — ensure_stream() retries on demand.
        if let Err(e) = inner.open_stream() {
            tracing::warn!("Audio output unavailable at startup ({}); will open on demand", e);
        }
        inner
    }

    /// (Re)open the default output stream, dropping any existing one first so the
    /// device handle is released before reacquiring it. The ALSA `default` is pinned
    /// to the USB DAC via `/etc/asound.conf`, so this reacquires card 2 once it has
    /// re-enumerated.
    fn open_stream(&mut self) -> anyhow::Result<()> {
        self.stream = None;
        self.stream_handle = None;
        let (stream, handle) = OutputStream::try_default()?;
        self.stream = Some(stream);
        self.stream_handle = Some(handle);
        tracing::info!("Audio output stream opened");
        Ok(())
    }

    /// End-to-end liveness check: append a brief silent buffer to a throwaway sink
    /// and confirm it drains. A live device's cpal callback pulls it within tens of
    /// milliseconds; a removed/stalled device (e.g. amp powered off after the stream
    /// was opened) never pulls, so the sink stays full and we report dead. This is
    /// the only reliable signal — rodio/cpal silently swallow device removal, so
    /// `Sink::try_new` and decode keep succeeding into a dead stream.
    fn probe_alive(&self) -> bool {
        let handle = match self.stream_handle.as_ref() {
            Some(h) => h,
            None => return false,
        };
        let sink = match Sink::try_new(handle) {
            Ok(s) => s,
            Err(_) => return false,
        };
        // ~20 ms of mono silence at 44.1 kHz — inaudible, no click.
        sink.append(SamplesBuffer::new(1, 44100, vec![0.0f32; 882]));
        let deadline = Instant::now() + Duration::from_millis(250);
        while !sink.empty() {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }

    /// Guarantee a healthy output stream before playback. Fast path: stream present
    /// and probe passes → return. Otherwise (re)open the device, re-probe, and on
    /// success resume any active alarms onto the fresh handle. Returns an error when
    /// the device is genuinely unavailable, so the playback RPC fails cleanly instead
    /// of silently dropping audio into the void.
    fn ensure_stream(&mut self) -> anyhow::Result<()> {
        if self.stream_handle.is_some() && self.probe_alive() {
            return Ok(());
        }

        tracing::info!("Audio output not healthy; reopening stream");
        self.open_stream()?;
        if !self.probe_alive() {
            // Opened but not draining — device not actually ready (e.g. ALSA default
            // resolved but the USB DAC is still absent). Clear so we retry next time.
            self.stream = None;
            self.stream_handle = None;
            anyhow::bail!("audio output not draining after reopen (device unavailable)");
        }
        self.resume_alarms();
        Ok(())
    }

    /// Recreate every active alarm sink on the current (freshly opened) handle. The
    /// old sinks were bound to the dropped stream and are silent; this re-arms them.
    fn resume_alarms(&mut self) {
        if self.active_alarms.is_empty() {
            return;
        }
        let handle = match self.stream_handle.clone() {
            Some(h) => h,
            None => return,
        };
        let ids: Vec<String> = self.active_alarms.keys().cloned().collect();
        for id in ids {
            let (path, volume) = match self.active_alarms.get(&id) {
                Some(s) => (s.path.clone(), s.volume),
                None => continue,
            };
            match make_alarm_sink(&handle, &path, volume) {
                Ok(sink) => {
                    if let Some(state) = self.active_alarms.get_mut(&id) {
                        state.sink.stop();
                        state.sink = sink;
                        state.started_at = Instant::now();
                    }
                    tracing::info!("Resumed alarm '{}' on reopened audio stream", id);
                }
                Err(e) => tracing::error!("Failed to resume alarm '{}': {}", id, e),
            }
        }
    }

    fn run(&mut self, mut command_rx: mpsc::UnboundedReceiver<AudioCommand>) {
        let mut last_cleanup = Instant::now();

        loop {
            // Try to receive a command with a non-blocking check
            match command_rx.try_recv() {
                Ok(command) => {
                    match command {
                        AudioCommand::PlayFile { path, volume, response } => {
                            let result = self.play_file_inner(&path, volume);
                            let _ = response.send(result);
                        }
                        AudioCommand::PlayBytes { data, volume, blocking, response } => {
                            let result = if blocking {
                                self.play_bytes_blocking_inner(data, volume)
                            } else {
                                self.play_bytes_inner(data, volume)
                            };
                            let _ = response.send(result);
                        }
                        AudioCommand::StartAlarm {
                            alarm_id,
                            path,
                            volume,
                            response,
                        } => {
                            let result = self.start_alarm_inner(alarm_id, &path, volume);
                            let _ = response.send(result);
                        }
                        AudioCommand::StopAlarm {
                            alarm_id,
                            response,
                        } => {
                            let result = self.stop_alarm_inner(&alarm_id);
                            let _ = response.send(result);
                        }
                        AudioCommand::DuckAlarms { factor, response } => {
                            self.duck_alarms_inner(factor);
                            let _ = response.send(());
                        }
                        AudioCommand::RestoreAlarms { response } => {
                            self.restore_alarms_inner();
                            let _ = response.send(());
                        }
                    }
                }
                Err(mpsc::error::TryRecvError::Empty) => {
                    // No command available; periodically maintain state.
                    if last_cleanup.elapsed() >= Duration::from_secs(3) {
                        // If an alarm is sounding, proactively heal the stream so a
                        // blaring klaxon resumes within seconds of the amp/DAC being
                        // powered back on, without waiting for the next RPC. When
                        // idle we skip the probe — the next playback heals on demand.
                        if !self.active_alarms.is_empty() {
                            if let Err(e) = self.ensure_stream() {
                                tracing::debug!("Audio still unavailable while alarm active: {}", e);
                            }
                        }
                        self.cleanup_dead_alarms();
                        last_cleanup = Instant::now();
                    }
                    // Sleep briefly to avoid busy-waiting
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    tracing::info!("Audio command channel closed, shutting down");
                    break;
                }
            }
        }
    }

    fn play_file_inner(&mut self, path: &PathBuf, volume: f32) -> anyhow::Result<()> {
        self.ensure_stream()?;
        let handle = self.stream_handle.as_ref().unwrap();
        let file = File::open(path)?;
        let source = Decoder::new(BufReader::new(file))?;
        let sink = Sink::try_new(handle)?;
        sink.set_volume(volume);
        sink.append(source);
        sink.detach();
        Ok(())
    }

    fn play_bytes_inner(&mut self, data: Vec<u8>, volume: f32) -> anyhow::Result<()> {
        self.ensure_stream()?;
        let handle = self.stream_handle.as_ref().unwrap();
        let cursor = std::io::Cursor::new(data);
        let source = Decoder::new(cursor)?;
        let sink = Sink::try_new(handle)?;
        sink.set_volume(volume);
        sink.append(source);
        sink.detach();
        Ok(())
    }

    /// Identical to `play_bytes_inner` but blocks the audio thread until the clip
    /// finishes, so the caller's oneshot — and thus the RPC — returns only after
    /// playback completes. Uses a bounded poll on `sink.empty()` rather than
    /// `sink.sleep_until_end()`: if the device stalls or vanishes mid-clip the cpal
    /// callback stops draining and `sleep_until_end()` would block the audio thread
    /// forever (wedging all subsequent commands). The 120 s cap sits well above any
    /// real clip length. Note: this still defers other audio commands (e.g.
    /// StopAlarm) by up to the clip length; the looping klaxon sink keeps sounding.
    fn play_bytes_blocking_inner(&mut self, data: Vec<u8>, volume: f32) -> anyhow::Result<()> {
        self.ensure_stream()?;
        let handle = self.stream_handle.as_ref().unwrap();
        let cursor = std::io::Cursor::new(data);
        let source = Decoder::new(cursor)?;
        let sink = Sink::try_new(handle)?;
        sink.set_volume(volume);
        sink.append(source);

        let deadline = Instant::now() + Duration::from_secs(120);
        while !sink.empty() {
            if Instant::now() >= deadline {
                tracing::warn!("Blocking playback exceeded 120s cap; abandoning (audio device stalled?)");
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }

    fn start_alarm_inner(&mut self, alarm_id: String, path: &PathBuf, volume: f32) -> anyhow::Result<()> {
        self.ensure_stream()?;
        let handle = self.stream_handle.as_ref().unwrap().clone();
        let sink = make_alarm_sink(&handle, path, volume)?;

        // Stop existing alarm with same ID if present
        if let Some(old_state) = self.active_alarms.remove(&alarm_id) {
            old_state.sink.stop();
        }

        let alarm_state = AlarmState {
            sink,
            path: path.clone(),
            volume,
            started_at: Instant::now(),
        };

        self.active_alarms.insert(alarm_id, alarm_state);
        Ok(())
    }

    fn stop_alarm_inner(&mut self, alarm_id: &str) -> bool {
        if let Some(state) = self.active_alarms.remove(alarm_id) {
            state.sink.stop();
            true
        } else {
            false
        }
    }

    /// Scale every active alarm sink by `factor` (e.g. 0.5 = half) WITHOUT mutating
    /// the stored canonical `AlarmState.volume`, so `restore_alarms_inner` can put
    /// it back.
    fn duck_alarms_inner(&self, factor: f32) {
        if self.active_alarms.is_empty() {
            return;
        }
        for (alarm_id, state) in &self.active_alarms {
            let ducked = state.volume * factor;
            state.sink.set_volume(ducked);
            tracing::debug!("Ducked alarm '{}' by factor {} to volume {}", alarm_id, factor, ducked);
        }
    }

    /// Restore every active alarm sink to its stored canonical volume.
    fn restore_alarms_inner(&self) {
        if self.active_alarms.is_empty() {
            return;
        }
        for (alarm_id, state) in &self.active_alarms {
            state.sink.set_volume(state.volume);
            tracing::debug!("Restored alarm '{}' to volume {}", alarm_id, state.volume);
        }
    }

    fn cleanup_dead_alarms(&mut self) {
        let mut dead_alarms = Vec::new();

        for (alarm_id, state) in &self.active_alarms {
            // Check if the sink is empty (which it shouldn't be for infinite playback)
            if state.sink.empty() {
                tracing::warn!(
                    "Alarm '{}' sink became empty after {:?} - this indicates an audio stream error",
                    alarm_id,
                    state.started_at.elapsed()
                );
                dead_alarms.push(alarm_id.clone());
            }
        }

        // Remove and attempt to restart dead alarms
        for alarm_id in dead_alarms {
            if let Some(state) = self.active_alarms.remove(&alarm_id) {
                tracing::info!(
                    "Attempting to restart alarm '{}' after audio stream failure",
                    alarm_id
                );

                // Try to restart the alarm
                match self.start_alarm_inner(alarm_id.clone(), &state.path, state.volume) {
                    Ok(_) => {
                        tracing::info!("Successfully restarted alarm '{}'", alarm_id);
                    }
                    Err(e) => {
                        tracing::error!(
                            "Failed to restart alarm '{}': {}. Audio device may be unavailable.",
                            alarm_id,
                            e
                        );
                    }
                }
            }
        }
    }
}
