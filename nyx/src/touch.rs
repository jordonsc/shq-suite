use anyhow::{anyhow, Result};
use evdev::{Device, EventType, InputEventKind};
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{watch, Mutex};
use tokio::task;

/// Touch monitor for detecting touch events
#[derive(Clone)]
pub struct TouchMonitor {
    last_touch: Arc<Mutex<f64>>,
    shutdown: watch::Sender<bool>,
    should_block: Arc<Mutex<bool>>,
    wake_tx: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<()>>>>,
}

impl TouchMonitor {
    /// Create a new touch monitor
    pub fn new() -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();

        let (shutdown_tx, _) = watch::channel(false);

        Self {
            last_touch: Arc::new(Mutex::new(now)),
            shutdown: shutdown_tx,
            should_block: Arc::new(Mutex::new(false)),
            wake_tx: Arc::new(Mutex::new(None)),
        }
    }

    /// Set a wake callback that gets called when touch is detected while blocking
    pub async fn set_wake_callback(&self, tx: tokio::sync::mpsc::UnboundedSender<()>) {
        *self.wake_tx.lock().await = Some(tx);
    }

    /// Set whether touch events should be blocked (grabbed)
    pub async fn set_should_block(&self, block: bool) {
        tracing::debug!("Touch event blocking: {}", if block { "enabled" } else { "disabled" });
        *self.should_block.lock().await = block;
    }

    /// Start monitoring for touch events
    pub async fn start(&self) -> Result<()> {
        let device_path = Self::find_touch_device().await?;
        tracing::info!("Touch monitor started on device: {:?}", device_path);

        let last_touch = self.last_touch.clone();
        let should_block = self.should_block.clone();
        let wake_tx = self.wake_tx.clone();
        let mut shutdown_rx = self.shutdown.subscribe();

        task::spawn(async move {
            loop {
                // Check for shutdown signal with timeout to avoid blocking
                if tokio::time::timeout(
                    tokio::time::Duration::from_millis(100),
                    shutdown_rx.changed()
                ).await.is_ok() && *shutdown_rx.borrow() {
                    tracing::info!("Touch monitor shutting down");
                    break;
                }

                // Open the device (may need to reopen if disconnected)
                let device = match Device::open(&device_path) {
                    Ok(dev) => dev,
                    Err(e) => {
                        tracing::warn!("Failed to open touch device: {}, retrying in 5s", e);
                        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                        continue;
                    }
                };

                // Set device to non-blocking mode
                let fd = device.as_raw_fd();
                if let Err(e) = fcntl(fd, FcntlArg::F_SETFL(OFlag::O_NONBLOCK)) {
                    tracing::error!("Failed to set device to non-blocking: {}", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    continue;
                }

                // Monitor events
                if let Err(e) = Self::monitor_events(
                    device,
                    last_touch.clone(),
                    should_block.clone(),
                    wake_tx.clone(),
                    &mut shutdown_rx,
                )
                .await
                {
                    tracing::error!("Touch monitor error: {}, restarting in 5s", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
            }
        });

        Ok(())
    }

    /// Stop the touch monitor
    pub fn stop(&self) {
        let _ = self.shutdown.send(true);
    }

    /// Monitor events from the device
    async fn monitor_events(
        mut device: Device,
        last_touch: Arc<Mutex<f64>>,
        should_block: Arc<Mutex<bool>>,
        wake_tx: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<()>>>>,
        shutdown_rx: &mut watch::Receiver<bool>,
    ) -> Result<()> {
        let mut is_grabbed = false;

        loop {
            // Check for shutdown
            if *shutdown_rx.borrow() {
                // Ungrab before exiting
                if is_grabbed {
                    let _ = device.ungrab();
                }
                break;
            }

            // Check if we should grab/ungrab the device
            let should_be_grabbed = *should_block.lock().await;
            if should_be_grabbed && !is_grabbed {
                tracing::info!("Grabbing touch device to block events");
                device.grab().map_err(|e| anyhow!("Failed to grab device: {}", e))?;
                is_grabbed = true;
            } else if !should_be_grabbed && is_grabbed {
                tracing::info!("Ungrabbing touch device to allow events");
                device.ungrab().map_err(|e| anyhow!("Failed to ungrab device: {}", e))?;
                is_grabbed = false;
            }

            // Fetch events (non-blocking)
            match device.fetch_events() {
                Ok(events) => {
                    // Process events
                    for event in events {
                        match event.kind() {
                            InputEventKind::AbsAxis(_) | InputEventKind::Key(_) => {
                                // Touch event detected
                                let now = SystemTime::now()
                                    .duration_since(UNIX_EPOCH)
                                    .unwrap()
                                    .as_secs_f64();

                                *last_touch.lock().await = now;
                                tracing::debug!("Touch event detected (blocking={})", is_grabbed);

                                // If we're blocking events, trigger wake callback
                                if is_grabbed {
                                    if let Some(tx) = wake_tx.lock().await.as_ref() {
                                        tracing::info!("Touch detected while screen off, triggering wake");
                                        let _ = tx.send(());
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Err(e) => {
                    // EAGAIN/EWOULDBLOCK is expected for non-blocking reads with no data
                    if e.kind() != std::io::ErrorKind::WouldBlock {
                        return Err(anyhow!("Failed to fetch events: {}", e));
                    }
                }
            }

            // Sleep briefly to avoid busy-waiting
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }

        Ok(())
    }

    /// Find the touch input device
    async fn find_touch_device() -> Result<PathBuf> {
        let devices = evdev::enumerate();

        // Look for touchscreen device
        for (path, device) in devices {
            let name = device.name().unwrap_or("");

            // Check if this is a touchscreen
            if name.to_lowercase().contains("touch")
                || name.to_lowercase().contains("ft5406")
                || device
                    .supported_events()
                    .contains(EventType::ABSOLUTE)
            {
                tracing::info!("Found touch device: {} at {:?}", name, path);
                return Ok(path);
            }
        }

        Err(anyhow!("No touch device found"))
    }

    /// Get the timestamp of the last touch event
    pub async fn get_last_touch_time(&self) -> f64 {
        *self.last_touch.lock().await
    }

    /// Get idle time in seconds
    pub async fn get_idle_time(&self) -> f64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let last_touch = *self.last_touch.lock().await;
        now - last_touch
    }

    /// Reset the touch timer
    pub async fn reset_touch_timer(&self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        *self.last_touch.lock().await = now;
    }
}
