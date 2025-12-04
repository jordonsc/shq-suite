use anyhow::Result;
use std::sync::Arc;
use tokio::sync::{watch, Mutex};
use tokio::task;
use tokio::time::{interval, Duration};

use crate::display::DisplayController;
use crate::messages::{AutoDimConfig, AutoDimStatus};
use crate::touch::TouchMonitor;

/// Auto-dim manager for automatic brightness dimming and display power-off
#[derive(Clone)]
pub struct AutoDimManager {
    config: Arc<Mutex<AutoDimConfig>>,
    is_dimmed: Arc<Mutex<bool>>,
    display: DisplayController,
    touch_monitor: TouchMonitor,
    shutdown: watch::Sender<bool>,
}

impl AutoDimManager {
    /// Create a new auto-dim manager
    pub fn new(
        config: AutoDimConfig,
        display: DisplayController,
        touch_monitor: TouchMonitor,
    ) -> Self {
        let (shutdown_tx, _) = watch::channel(false);

        Self {
            config: Arc::new(Mutex::new(config)),
            is_dimmed: Arc::new(Mutex::new(false)),
            display,
            touch_monitor,
            shutdown: shutdown_tx,
        }
    }

    /// Start the auto-dim manager
    pub async fn start(&self) -> Result<()> {
        tracing::info!("Auto-dim manager started");

        // Create wake channel for touch monitor callbacks
        let (wake_tx, mut wake_rx) = tokio::sync::mpsc::unbounded_channel();
        self.touch_monitor.set_wake_callback(wake_tx).await;

        let config = self.config.clone();
        let is_dimmed = self.is_dimmed.clone();
        let display = self.display.clone();
        let touch_monitor = self.touch_monitor.clone();
        let mut shutdown_rx = self.shutdown.subscribe();

        // Spawn wake handler (handles touch events and explicit wake calls)
        let wake_display = self.display.clone();
        let wake_config = self.config.clone();
        let wake_touch = self.touch_monitor.clone();
        task::spawn(async move {
            while let Some(()) = wake_rx.recv().await {
                tracing::info!("Wake request received");
                let cfg = wake_config.lock().await.clone();

                // Reset idle time
                wake_touch.reset_touch_timer().await;

                // Stop grabbing if grabbing
                wake_touch.set_should_block(false).await;

                // Restore brightness if below bright_level
                if let Ok(current_brightness) = wake_display.get_brightness().await {
                    if current_brightness < cfg.bright_level {
                        if let Err(e) = wake_display.set_brightness(cfg.bright_level).await {
                            tracing::error!("Failed to set brightness during wake: {}", e);
                        }
                    }
                }
            }
        });

        task::spawn(async move {
            // Check every 25ms for faster response to touch events
            let mut tick = interval(Duration::from_millis(25));

            loop {
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            tracing::info!("Auto-dim manager shutting down");
                            break;
                        }
                    }
                    _ = tick.tick() => {
                        if let Err(e) = Self::check_and_apply_dimming(
                            &config,
                            &is_dimmed,
                            &display,
                            &touch_monitor,
                        )
                        .await
                        {
                            tracing::error!("Auto-dim error: {}", e);
                        }
                    }
                }
            }
        });

        Ok(())
    }

    /// Stop the auto-dim manager
    pub fn stop(&self) {
        let _ = self.shutdown.send(true);
    }

    /// Check idle time and apply dimming/off logic
    async fn check_and_apply_dimming(
        config: &Arc<Mutex<AutoDimConfig>>,
        _is_dimmed: &Arc<Mutex<bool>>,
        display: &DisplayController,
        touch_monitor: &TouchMonitor,
    ) -> Result<()> {
        let cfg = config.lock().await.clone();
        let idle_time = touch_monitor.get_idle_time().await;

        // Check if auto-dim is enabled
        if cfg.auto_dim_time == 0 && cfg.auto_off_time == 0 {
            return Ok(());
        }

        // Auto-off: same as sleep
        if cfg.auto_off_time > 0 && idle_time >= cfg.auto_off_time as f64 {
            let current_brightness = display.get_brightness().await?;
            if current_brightness > 0 {
                tracing::info!("Auto-off triggered after {:.1} seconds idle", idle_time);
                display.set_brightness(0).await?;
                touch_monitor.set_should_block(true).await;
            }
            return Ok(());
        }

        // Auto-dim: set brightness to dim_level if currently brighter
        if cfg.auto_dim_time > 0 && idle_time >= cfg.auto_dim_time as f64 {
            let current_brightness = display.get_brightness().await?;
            if current_brightness > cfg.dim_level {
                tracing::info!(
                    "Auto-dim triggered after {:.1} seconds idle, setting brightness to {}",
                    idle_time,
                    cfg.dim_level
                );
                display.set_brightness(cfg.dim_level).await?;
            }
        }

        // On touch when dimmed (recent activity detected): restore brightness
        // Touch events update idle_time, so very low idle_time indicates a recent touch
        if idle_time < 0.1 {
            let current_brightness = display.get_brightness().await?;
            if current_brightness > 0 && current_brightness < cfg.bright_level {
                tracing::info!("Touch detected while dimmed, restoring brightness to {}", cfg.bright_level);
                display.set_brightness(cfg.bright_level).await?;
            }
        }

        Ok(())
    }

    /// Get current configuration
    pub async fn get_config(&self) -> AutoDimConfig {
        self.config.lock().await.clone()
    }

    /// Set configuration
    pub async fn set_config(&self, config: AutoDimConfig) {
        *self.config.lock().await = config;
    }

    /// Get current status
    pub async fn get_status(&self) -> AutoDimStatus {
        let config = self.config.lock().await.clone();
        let is_dimmed = *self.is_dimmed.lock().await;
        let last_touch_time = self.touch_monitor.get_last_touch_time().await;

        AutoDimStatus {
            dim_level: config.dim_level,
            bright_level: config.bright_level,
            auto_dim_time: config.auto_dim_time,
            auto_off_time: config.auto_off_time,
            is_dimmed,
            last_touch_time,
        }
    }

    /// Reset dimmed state and idle timer (call after manual brightness changes)
    pub async fn reset_dimmed_state(&self) {
        *self.is_dimmed.lock().await = false;
        self.touch_monitor.reset_touch_timer().await;
    }

    /// Wake the display (turn on and set to bright level)
    pub async fn wake(&self) -> Result<()> {
        let config = self.config.lock().await.clone();

        // Reset idle time
        self.touch_monitor.reset_touch_timer().await;

        // Stop grabbing if grabbing
        self.touch_monitor.set_should_block(false).await;

        // Restore brightness if below bright_level
        let current_brightness = self.display.get_brightness().await?;
        if current_brightness < config.bright_level {
            self.display.set_brightness(config.bright_level).await?;
        }

        tracing::info!("Display woken");
        Ok(())
    }

    /// Sleep the display (turn off)
    pub async fn sleep(&self) -> Result<()> {
        // Start grabbing
        self.touch_monitor.set_should_block(true).await;

        // Set brightness to 0
        self.display.set_brightness(0).await?;

        tracing::info!("Display put to sleep");
        Ok(())
    }
}
