use anyhow::Result;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::process::Child;
use tokio::sync::{watch, Mutex};
use tokio::task;
use tokio::time::{interval, Duration};

use crate::display::DisplayController;
use crate::messages::{AutoDimConfig, AutoDimStatus, IdleMode};
use crate::touch::TouchMonitor;

/// Handle to the running Chronos clock overlay (None when not shown).
type ClockHandle = Arc<Mutex<Option<Child>>>;

/// Auto-dim manager for automatic brightness dimming and display power-off
#[derive(Clone)]
pub struct AutoDimManager {
    config: Arc<Mutex<AutoDimConfig>>,
    is_dimmed: Arc<Mutex<bool>>,
    display: DisplayController,
    touch_monitor: TouchMonitor,
    shutdown: watch::Sender<bool>,
    clock: ClockHandle,
    /// When set, the idle/auto-dim loop must NOT blank the backlight or spawn the
    /// Chronos clock overlay — the screen is PINNED awake (e.g. a live alarm HUD
    /// takeover). Clearing it lets normal idle dimming/blanking resume.
    pinned_awake: Arc<AtomicBool>,
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
            clock: Arc::new(Mutex::new(None)),
            pinned_awake: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Pin the display awake (or release the pin). While pinned, the idle loop
    /// will not dim, blank, or spawn the clock overlay. Releasing it lets normal
    /// auto-dim/idle behaviour resume immediately.
    pub fn set_pinned_awake(&self, pinned: bool) {
        let was = self.pinned_awake.swap(pinned, Ordering::SeqCst);
        if was != pinned {
            tracing::info!(pinned, "Display keep-awake pin changed");
        }
    }

    /// Resolve the Chronos binary, expected alongside the nyx executable.
    fn chronos_path() -> PathBuf {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("chronos")))
            .unwrap_or_else(|| PathBuf::from("chronos"))
    }

    /// Spawn the Chronos clock overlay. It draws on the wlr-layer-shell overlay
    /// layer, above the fullscreen Chromium kiosk.
    async fn spawn_clock() -> Option<Child> {
        let path = Self::chronos_path();
        let mut cmd = tokio::process::Command::new(&path);
        cmd.kill_on_drop(true);
        // Inherit the session env; backfill the Wayland socket if the unit
        // didn't export it.
        if std::env::var_os("WAYLAND_DISPLAY").is_none() {
            cmd.env("WAYLAND_DISPLAY", "wayland-0");
        }
        match cmd.spawn() {
            Ok(child) => {
                tracing::info!("Spawned clock overlay ({})", path.display());
                Some(child)
            }
            Err(e) => {
                tracing::error!("Failed to spawn clock overlay {}: {}", path.display(), e);
                None
            }
        }
    }

    /// Dismiss the clock overlay if it is up (SIGKILL → surface torn down,
    /// revealing the live dashboard instantly).
    async fn kill_clock(clock: &ClockHandle) {
        let mut guard = clock.lock().await;
        if let Some(mut child) = guard.take() {
            let _ = child.kill().await;
            tracing::info!("Clock overlay dismissed");
        }
    }

    /// Start the auto-dim manager
    pub async fn start(&self) -> Result<()> {
        tracing::info!("Auto-dim manager started");

        // Kill any clock overlay orphaned by a previous nyx (e.g. a hard
        // restart that skipped kill_on_drop), so we never stack overlays.
        let _ = tokio::process::Command::new("pkill")
            .args(["-x", "chronos"])
            .status()
            .await;

        // Create wake channel for touch monitor callbacks
        let (wake_tx, mut wake_rx) = tokio::sync::mpsc::unbounded_channel();
        self.touch_monitor.set_wake_callback(wake_tx).await;

        let config = self.config.clone();
        let is_dimmed = self.is_dimmed.clone();
        let display = self.display.clone();
        let touch_monitor = self.touch_monitor.clone();
        let clock = self.clock.clone();
        let pinned_awake = self.pinned_awake.clone();
        let mut shutdown_rx = self.shutdown.subscribe();

        // Spawn wake handler (handles touch events and explicit wake calls)
        let wake_display = self.display.clone();
        let wake_config = self.config.clone();
        let wake_touch = self.touch_monitor.clone();
        let wake_clock = self.clock.clone();
        task::spawn(async move {
            while let Some(()) = wake_rx.recv().await {
                tracing::info!("Wake request received");
                let cfg = wake_config.lock().await.clone();

                // Dismiss the clock overlay (no-op if it isn't up)
                Self::kill_clock(&wake_clock).await;

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
                            &clock,
                            &pinned_awake,
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
        clock: &ClockHandle,
        pinned_awake: &Arc<AtomicBool>,
    ) -> Result<()> {
        let cfg = config.lock().await.clone();
        let idle_time = touch_monitor.get_idle_time().await;

        // Pinned awake (e.g. a live alarm HUD takeover): never dim, blank, or
        // spawn the clock. The wake side already restored brightness + ungrabbed,
        // so there is nothing to do here until the pin is released.
        if pinned_awake.load(Ordering::SeqCst) {
            return Ok(());
        }

        // Check if auto-dim is enabled
        if cfg.auto_dim_time == 0 && cfg.auto_off_time == 0 {
            return Ok(());
        }

        // Auto-off point: either blank the screen, or (clock mode) show the
        // Chronos overlay held at dim_level. In both cases we grab touch so the
        // wake tap is consumed rather than reaching Chrome.
        if cfg.auto_off_time > 0 && idle_time >= cfg.auto_off_time as f64 {
            match cfg.idle_mode {
                IdleMode::Off => {
                    let current_brightness = display.get_brightness().await?;
                    if current_brightness > 0 {
                        tracing::info!("Auto-off triggered after {:.1} seconds idle", idle_time);
                        display.set_brightness(0).await?;
                        touch_monitor.set_should_block(true).await;
                    }
                }
                IdleMode::Clock => {
                    let mut guard = clock.lock().await;
                    // Clear a handle whose process has already exited so we respawn.
                    if let Some(child) = guard.as_mut() {
                        if matches!(child.try_wait(), Ok(Some(_))) {
                            *guard = None;
                        }
                    }
                    if guard.is_none() {
                        tracing::info!(
                            "Clock screensaver triggered after {:.1} seconds idle",
                            idle_time
                        );
                        *guard = Self::spawn_clock().await;
                        display.set_brightness(cfg.dim_level).await?;
                        touch_monitor.set_should_block(true).await;
                    }
                }
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
            idle_mode: config.idle_mode,
        }
    }

    /// Reset dimmed state and idle timer (call after manual brightness changes)
    pub async fn reset_dimmed_state(&self) {
        *self.is_dimmed.lock().await = false;
        // A manual brightness change implies the screensaver is over.
        Self::kill_clock(&self.clock).await;
        self.touch_monitor.reset_touch_timer().await;
        // Without this, an API-driven wake (set_display true / set_brightness >0)
        // leaves the device grabbed and the first tap gets consumed as a wake-trigger.
        self.touch_monitor.set_should_block(false).await;
    }

    /// Wake the display (turn on and set to bright level)
    pub async fn wake(&self) -> Result<()> {
        let config = self.config.lock().await.clone();

        // Dismiss the clock overlay if it's up
        Self::kill_clock(&self.clock).await;

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

    /// Sleep the display (turn off). An explicit sleep always blanks the
    /// screen, even in clock mode — the clock is only the idle screensaver.
    pub async fn sleep(&self) -> Result<()> {
        // Dismiss the clock overlay if it's up
        Self::kill_clock(&self.clock).await;

        // Start grabbing
        self.touch_monitor.set_should_block(true).await;

        // Set brightness to 0
        self.display.set_brightness(0).await?;

        tracing::info!("Display put to sleep");
        Ok(())
    }
}
