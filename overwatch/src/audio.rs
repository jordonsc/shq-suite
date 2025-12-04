use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
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
}

pub struct AudioManager {
    command_tx: mpsc::UnboundedSender<AudioCommand>,
}

struct AudioManagerInner {
    _stream: OutputStream,
    stream_handle: OutputStreamHandle,
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

        // Spawn dedicated audio thread
        std::thread::spawn(move || {
            let mut inner = match AudioManagerInner::new() {
                Ok(inner) => inner,
                Err(e) => {
                    tracing::error!("Failed to initialize audio: {}", e);
                    return;
                }
            };

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

    pub async fn play_bytes(&self, data: Vec<u8>, volume: f32) -> anyhow::Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(AudioCommand::PlayBytes {
                data,
                volume,
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
}

impl AudioManagerInner {
    fn new() -> anyhow::Result<Self> {
        let (stream, stream_handle) = OutputStream::try_default()?;
        Ok(Self {
            _stream: stream,
            stream_handle,
            active_alarms: HashMap::new(),
        })
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
                        AudioCommand::PlayBytes { data, volume, response } => {
                            let result = self.play_bytes_inner(data, volume);
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
                    }
                }
                Err(mpsc::error::TryRecvError::Empty) => {
                    // No command available, check if we need to do cleanup
                    if last_cleanup.elapsed() >= Duration::from_secs(10) {
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

    fn play_file_inner(&self, path: &PathBuf, volume: f32) -> anyhow::Result<()> {
        let file = File::open(path)?;
        let source = Decoder::new(BufReader::new(file))?;
        let sink = Sink::try_new(&self.stream_handle)?;
        sink.set_volume(volume);
        sink.append(source);
        sink.detach();
        Ok(())
    }

    fn play_bytes_inner(&self, data: Vec<u8>, volume: f32) -> anyhow::Result<()> {
        let cursor = std::io::Cursor::new(data);
        let source = Decoder::new(cursor)?;
        let sink = Sink::try_new(&self.stream_handle)?;
        sink.set_volume(volume);
        sink.append(source);
        sink.detach();
        Ok(())
    }

    fn start_alarm_inner(&mut self, alarm_id: String, path: &PathBuf, volume: f32) -> anyhow::Result<()> {
        let file = File::open(path)?;
        let source = Decoder::new(BufReader::new(file))?.repeat_infinite();

        let sink = Sink::try_new(&self.stream_handle)?;
        sink.set_volume(volume);
        sink.append(source);

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
