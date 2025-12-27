use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex, RwLock};
use tokio::time::{interval, Duration};

use crate::cnc::CncController;
use crate::config::DoorConfig;
use crate::messages::{DoorState, DoorStatus};

/// Door controller that manages door state and CNC movements
pub struct DoorController {
    cnc: Arc<RwLock<Arc<CncController>>>,
    config: Arc<RwLock<DoorConfig>>,
    status: Arc<Mutex<DoorStatus>>,
    is_homed: Arc<Mutex<bool>>,
    home_position: Arc<Mutex<f64>>, // MPos when we set home (for calculating relative position)
    stop_requested: Arc<Mutex<bool>>,
    auto_home_done: Arc<Mutex<bool>>, // Tracks if auto-home has been performed
    discard_next_poll: Arc<Mutex<bool>>, // Flag to discard next status poll (set when state is updated manually)
    status_tx: broadcast::Sender<DoorStatus>, // Broadcasts status changes
}

impl DoorController {
    /// Calculate position as percentage (0-100), capped at bounds
    fn calculate_position_percent(position_mm: f64, open_distance: f64) -> f64 {
        let abs_open = open_distance.abs();
        if abs_open == 0.0 {
            return 0.0;
        }

        let percent = (position_mm.abs() / abs_open) * 100.0;
        percent.max(0.0).min(100.0)
    }

    /// Create a new door controller
    pub async fn new(cnc: CncController, config: DoorConfig) -> Result<Self> {
        let (status_tx, _) = broadcast::channel(100);

        let controller = Self {
            cnc: Arc::new(RwLock::new(Arc::new(cnc))),
            config: Arc::new(RwLock::new(config)),
            status: Arc::new(Mutex::new(DoorStatus {
                state: DoorState::Pending,
                position_mm: 0.0,
                position_percent: 0.0,
                fault_message: None,
                alarm_code: None,
            })),
            is_homed: Arc::new(Mutex::new(false)),
            home_position: Arc::new(Mutex::new(0.0)),
            stop_requested: Arc::new(Mutex::new(false)),
            auto_home_done: Arc::new(Mutex::new(false)),
            discard_next_poll: Arc::new(Mutex::new(false)),
            status_tx,
        };

        // Start background position monitoring
        controller.start_position_monitor();

        Ok(controller)
    }

    /// Subscribe to status updates
    pub fn subscribe_status(&self) -> broadcast::Receiver<DoorStatus> {
        self.status_tx.subscribe()
    }

    /// Create a door controller in fault state (when initialization fails)
    pub fn new_fault(error: String, config: DoorConfig) -> Self {
        let (status_tx, _) = broadcast::channel(100);

        let controller = Self {
            cnc: Arc::new(RwLock::new(Arc::new(CncController::dummy()))),
            config: Arc::new(RwLock::new(config)),
            status: Arc::new(Mutex::new(DoorStatus {
                state: DoorState::Fault,
                position_mm: 0.0,
                position_percent: 0.0,
                fault_message: Some(error),
                alarm_code: None,
            })),
            is_homed: Arc::new(Mutex::new(false)),
            home_position: Arc::new(Mutex::new(0.0)),
            stop_requested: Arc::new(Mutex::new(false)),
            auto_home_done: Arc::new(Mutex::new(false)),
            discard_next_poll: Arc::new(Mutex::new(false)),
            status_tx,
        };

        // Start position monitor - it will skip monitoring while in fault state
        // but will automatically activate when reconnect() clears the fault
        controller.start_position_monitor();

        controller
    }

    /// Set fault state
    pub async fn set_fault(&self, error: String) {
        let mut status = self.status.lock().await;
        status.state = DoorState::Fault;
        status.fault_message = Some(error.clone());
        tracing::error!("System entered fault state: {}", error);
    }

    /// Clear fault state and update CNC connection
    pub async fn reconnect(&self, cnc: CncController, config: DoorConfig) -> Result<()> {
        // Update the CNC controller
        let mut cnc_lock = self.cnc.write().await;
        *cnc_lock = Arc::new(cnc);
        drop(cnc_lock);

        // Update config
        let mut cfg = self.config.write().await;
        *cfg = config;
        drop(cfg);

        // Clear fault state and reset homing
        let mut status = self.status.lock().await;
        status.state = DoorState::Pending;
        status.fault_message = None;
        drop(status);

        let mut is_homed = self.is_homed.lock().await;
        *is_homed = false; // Reset homed state on reconnect

        tracing::info!("System reconnected successfully - fault state cleared");
        Ok(())
    }

    /// Attempt to reconnect to CNC controller (called on-demand when commands fail)
    async fn try_reconnect(&self) -> Result<()> {
        tracing::info!("Attempting to reconnect to CNC controller...");

        // Close the old connection explicitly (important for serial ports)
        {
            let cnc = self.cnc.read().await;
            cnc.close().await;
        }

        // Get current config
        let config = self.config.read().await.clone();

        // Try to create new CNC connection
        let cnc = CncController::new(&config.cnc_connection)
            .await
            .context("Failed to create new CNC connection")?;

        // Reconnect
        self.reconnect(cnc, config).await?;

        tracing::info!("Reconnection successful");
        Ok(())
    }

    /// Execute a command with automatic reconnection on connection errors
    ///
    /// This helper method:
    /// 1. Executes the provided async function
    /// 2. If it fails, checks if the error is a connection error
    /// 3. Only reconnects on connection errors (not grblHAL command errors)
    /// 4. Retries the command once after successful reconnection
    ///
    /// # Arguments
    /// * `operation` - The async function to execute
    /// * `operation_name` - Name of the operation for logging
    async fn execute_with_reconnect<F, Fut, T>(
        &self,
        mut operation: F,
        operation_name: &str,
    ) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        match operation().await {
            Ok(result) => Ok(result),
            Err(e) => {
                // Only attempt reconnection on connection errors, not grblHAL errors
                if CncController::is_connection_error(&e) {
                    tracing::warn!(
                        "{} failed due to connection error: {}. Attempting reconnection...",
                        operation_name,
                        e
                    );

                    // Try to reconnect
                    if let Err(reconnect_err) = self.try_reconnect().await {
                        self.set_fault(format!("Failed to reconnect: {}", reconnect_err))
                            .await;
                        return Err(anyhow::anyhow!(
                            "{} failed and reconnection failed: {}",
                            operation_name,
                            reconnect_err
                        ));
                    }

                    // Retry the operation after successful reconnection
                    operation()
                        .await
                        .context(format!("{} failed after reconnection", operation_name))
                } else {
                    // grblHAL command error - don't reconnect, just return the error
                    tracing::debug!(
                        "{} failed with command error (not reconnecting): {}",
                        operation_name,
                        e
                    );
                    Err(e)
                }
            }
        }
    }

    /// Start background task to monitor position
    fn start_position_monitor(&self) {
        let cnc = self.cnc.clone();
        let config = self.config.clone();
        let status = self.status.clone();
        let is_homed = self.is_homed.clone();
        let home_position = self.home_position.clone();
        let discard_next_poll = self.discard_next_poll.clone();
        let status_tx = self.status_tx.clone();
        let auto_home_done = self.auto_home_done.clone();
        let door_controller = self.clone();

        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_millis(200));
            let mut last_broadcast_status: Option<DoorStatus> = None;

            loop {
                ticker.tick().await;

                // Skip polling during Homing (controller doesn't respond) and Fault (no connection)
                // Poll in all other states to detect alarms when idle
                {
                    let st = status.lock().await;

                    match st.state {
                        DoorState::Homing => {
                            // Don't poll during homing - controller doesn't respond to status queries
                            continue;
                        }
                        DoorState::Fault => {
                            // Don't poll when in fault state (no valid CNC connection)
                            continue;
                        }
                        _ => {
                            // Poll in all other states (Opening, Closing, Closed, Open, Intermediate, Pending, Halting)
                        }
                    }
                }

                // Query CNC status
                let cnc_read = cnc.read().await;
                if let Ok(status_str) = cnc_read.get_status().await {
                    // Check discard flag first - if set, skip this poll iteration
                    let mut discard = discard_next_poll.lock().await;
                    if *discard {
                        *discard = false;
                        drop(discard);
                        tracing::debug!("Discarding status poll due to discard flag");
                        continue;
                    }
                    drop(discard);

                    let cfg = config.read().await;
                    let homed = *is_homed.lock().await;
                    let mut st = status.lock().await;

                    // Re-check state after receiving response to avoid race conditions
                    // If state changed to Homing/Fault while we were waiting for CNC response, skip processing
                    match st.state {
                        DoorState::Homing => {
                            drop(st);
                            drop(cfg);
                            continue;
                        }
                        DoorState::Fault => {
                            drop(st);
                            drop(cfg);
                            continue;
                        }
                        _ => {}
                    }

                    // Check for alarm state
                    let (is_alarm, alarm_code) = CncController::parse_alarm(&status_str);

                    // Log alarm state changes
                    if is_alarm && st.state != DoorState::Alarm {
                        let alarm_msg = if let Some(code) = &alarm_code {
                            format!("CNC Alarm detected: Code {}", code)
                        } else {
                            "CNC Alarm detected".to_string()
                        };
                        tracing::warn!("{}", alarm_msg);
                    } else if !is_alarm && st.state == DoorState::Alarm {
                        tracing::info!("CNC Alarm cleared");
                    }

                    // If alarm detected, transition to Alarm state
                    if is_alarm {
                        st.state = DoorState::Alarm;
                        st.alarm_code = alarm_code;
                        continue;
                    }

                    // Clear alarm code if no alarm
                    st.alarm_code = None;

                    // Parse position (convert to relative by default)
                    // Note: We can't call self.parse_position() from the spawned task,
                    // so we inline the logic here
                    if let Ok(mpos) = CncController::parse_position(&status_str, &cfg.cnc_axis) {
                        if homed {
                            let home_pos = *home_position.lock().await;
                            st.position_mm = mpos - home_pos;
                            st.position_percent = Self::calculate_position_percent(st.position_mm, cfg.open_distance);
                            tracing::debug!("[Monitor] Position: MPos={}, HomePos={}, Relative={}", mpos, home_pos, st.position_mm);
                        } else {
                            st.position_mm = 0.0;
                            st.position_percent = 0.0;
                            tracing::debug!("[Monitor] Position: not homed, returning 0.0 (MPos={})", mpos);
                        }
                    }

                    // Update state based on CNC state
                    if let Ok(cnc_state) = CncController::parse_state(&status_str) {
                        match cnc_state.as_str() {
                            "Idle" => {
                                // Movement complete - determine final state based on position
                                if homed {
                                    let pos = st.position_mm;
                                    let prev_state = st.state.clone();

                                    // Calculate target open position based on direction
                                    let target_open_pos = if cfg.open_direction.to_lowercase() == "left" {
                                        -cfg.open_distance
                                    } else {
                                        cfg.open_distance
                                    };

                                    // Check if at closed position (within 0.1mm for floating point precision)
                                    if pos.abs() < 0.1 {
                                        st.state = DoorState::Closed;
                                        if prev_state == DoorState::Closing {
                                            tracing::info!("Door is in closed position");
                                        }
                                    }
                                    // Check if at open position (within 0.1mm for floating point precision)
                                    else if (pos - target_open_pos).abs() < 0.1 {
                                        st.state = DoorState::Open;
                                        if prev_state == DoorState::Opening {
                                            tracing::info!("Door is in open position");
                                        }
                                    }
                                    // Otherwise door is at an intermediate position
                                    else {
                                        st.state = DoorState::Intermediate;
                                        if prev_state == DoorState::Opening || prev_state == DoorState::Closing {
                                            tracing::info!("Door stopped at intermediate position: {} mm", pos);
                                        }
                                    }
                                } else {
                                    st.state = DoorState::Pending;
                                }
                            }
                            "Run" => {
                                // Keep current state (Opening/Closing/Homing)
                            }
                            "Home" => {
                                st.state = DoorState::Homing;
                            }
                            _ => {}
                        }
                    }

                    // Broadcast status if it changed
                    let current_status = st.clone();
                    drop(st); // Release lock before broadcasting

                    let should_broadcast = match &last_broadcast_status {
                        None => true,
                        Some(prev) => prev != &current_status,
                    };

                    if should_broadcast {
                        // Ignore send errors (no receivers)
                        let _ = status_tx.send(current_status.clone());
                        last_broadcast_status = Some(current_status.clone());
                    }

                    // Check for auto-home on first Pending state
                    if current_status.state == DoorState::Pending {
                        let mut auto_home_flag = auto_home_done.lock().await;
                        if !*auto_home_flag {
                            let cfg = config.read().await;
                            if cfg.auto_home {
                                tracing::info!("Auto-home enabled, starting homing sequence");
                                *auto_home_flag = true;
                                drop(auto_home_flag);
                                drop(cfg);

                                // Spawn home in background to avoid blocking the monitor
                                let controller = door_controller.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = controller.home().await {
                                        tracing::error!("Auto-home failed: {}", e);
                                    }
                                });
                            }
                        }
                    }
                }
            }
        });
    }

    /// Get current door status (returns cached status)
    pub async fn get_status(&self) -> DoorStatus {
        self.status.lock().await.clone()
    }

    /// Get raw status directly from CNC controller
    pub async fn get_raw_status(&self) -> Result<String> {
        let cnc = self.cnc.read().await;
        cnc.get_status().await
    }

    /// Parse position from status string and optionally convert to relative position
    ///
    /// # Arguments
    /// * `status_str` - The status string from the CNC controller
    /// * `convert_to_relative` - If true (default), converts MPos to relative position. Set to false only when recording home position.
    async fn parse_position(&self, status_str: &str, convert_to_relative: bool) -> Result<f64> {
        let config = self.config.read().await;
        let mpos = CncController::parse_position(status_str, &config.cnc_axis)?;

        if convert_to_relative {
            let is_homed = *self.is_homed.lock().await;
            if is_homed {
                let home_pos = *self.home_position.lock().await;
                let relative = mpos - home_pos;
                tracing::debug!("Position: MPos={}, HomePos={}, Relative={}", mpos, home_pos, relative);
                Ok(relative)
            } else {
                tracing::debug!("Position: not homed, returning 0.0 (MPos={})", mpos);
                Ok(0.0)
            }
        } else {
            tracing::debug!("Position: MPos={} (raw, not converted to relative)", mpos);
            Ok(mpos)
        }
    }

    /// Query CNC controller and update status, then return current status
    pub async fn query_and_get_status(&self) -> Result<DoorStatus> {
        // Query CNC controller with automatic reconnection on connection errors
        // Wrap the entire operation in a timeout to avoid blocking WebSocket
        let query_operation = async {
            let cnc = self.cnc.clone();
            let status_str = self
                .execute_with_reconnect(
                    move || {
                        let cnc = cnc.clone();
                        async move {
                            let cnc_read = cnc.read().await;
                            cnc_read.get_status().await
                        }
                    },
                    "Status query",
                )
                .await?;

            // Parse and update status
            self.parse_and_update_status(&status_str).await
        };

        match tokio::time::timeout(Duration::from_secs(3), query_operation).await {
            Ok(Ok(status)) => Ok(status),
            Ok(Err(e)) => {
                // Error occurred during query/reconnection
                tracing::error!("Status query failed: {}", e);
                let mut st = self.status.lock().await;
                st.state = DoorState::Fault;
                st.fault_message = Some(format!("Connection lost: {}", e));
                Ok(st.clone())
            }
            Err(_) => {
                // Timeout
                tracing::error!("Status query timed out after 3 seconds");
                let mut st = self.status.lock().await;
                st.state = DoorState::Fault;
                st.fault_message = Some("Status query timed out".to_string());
                Ok(st.clone())
            }
        }
    }

    /// Parse status string and update internal status
    async fn parse_and_update_status(&self, status_str: &str) -> Result<DoorStatus> {
        let homed = *self.is_homed.lock().await;
        let mut st = self.status.lock().await;

        // Check for alarm state
        let (is_alarm, alarm_code) = CncController::parse_alarm(&status_str);
        if is_alarm && st.state != DoorState::Alarm {
            tracing::warn!("CNC Alarm detected: Code {:?}", alarm_code);
        } else if !is_alarm && st.state == DoorState::Alarm {
            tracing::info!("CNC Alarm cleared");
        }

        // If alarm, set state and return
        if is_alarm {
            st.state = DoorState::Alarm;
            st.alarm_code = alarm_code;
            return Ok(st.clone());
        }

        // Clear alarm code if no alarm
        st.alarm_code = None;

        // Clear fault state if we were in fault and now successfully connected
        if st.state == DoorState::Fault {
            st.fault_message = None;
            tracing::info!("Connection recovered, clearing fault state");
        }

        // Parse position (convert to relative by default)
        drop(st); // Release lock before calling parse_position
        let position = self.parse_position(&status_str, true).await.unwrap_or(0.0);

        // Get config for state logic
        let cfg = self.config.read().await;

        let mut st = self.status.lock().await;
        st.position_mm = position;
        st.position_percent = Self::calculate_position_percent(position, cfg.open_distance);

        // Update state based on CNC state and position
        if let Ok(cnc_state) = CncController::parse_state(&status_str) {
            match cnc_state.as_str() {
                "Idle" => {
                    if homed {
                        let pos = st.position_mm;

                        // Calculate target open position based on direction
                        let target_open_pos = if cfg.open_direction.to_lowercase() == "left" {
                            -cfg.open_distance
                        } else {
                            cfg.open_distance
                        };

                        // Check if at closed position (within 0.1mm for floating point precision)
                        if pos.abs() < 0.1 {
                            st.state = DoorState::Closed;
                        }
                        // Check if at open position (within 0.1mm for floating point precision)
                        else if (pos - target_open_pos).abs() < 0.1 {
                            st.state = DoorState::Open;
                        }
                        // Otherwise door is at an intermediate position
                        else {
                            st.state = DoorState::Intermediate;
                        }
                    } else {
                        st.state = DoorState::Pending;
                    }
                }
                "Home" => {
                    st.state = DoorState::Homing;
                }
                _ => {}
            }
        }

        Ok(st.clone())
    }

    /// Update configuration
    pub async fn update_config(&self, config: DoorConfig) {
        let mut cfg = self.config.write().await;
        *cfg = config;
    }

    /// Get current configuration
    pub async fn get_config(&self) -> DoorConfig {
        self.config.read().await.clone()
    }

    /// Home the door (move to limit switch)
    pub async fn home(&self) -> Result<()> {
        {
            let status = self.status.lock().await;

            // Check if already homing
            if status.state == DoorState::Homing {
                return Ok(());
            }
        }

        // Always clear alarm before homing (soft reset + $X)
        // This ensures we can home even if an alarm occurred but wasn't detected
        tracing::info!("Clearing any potential alarms before homing");
        self.clear_alarm().await?;

        let config = self.config.read().await;

        // Set state to homing and discard any in-flight polls
        // Note: home_axis() blocks until complete, so state must be set BEFORE command
        let homing_status = {
            let mut discard = self.discard_next_poll.lock().await;
            *discard = true;
            drop(discard);

            let mut status = self.status.lock().await;
            status.state = DoorState::Homing;
            status.clone()
        };

        // Broadcast homing state to clients (position monitor won't broadcast during homing)
        let _ = self.status_tx.send(homing_status);

        tracing::info!("Homing door on {} axis", config.cnc_axis);

        // Send home command with automatic reconnection on connection errors
        // Note: home_axis() waits for homing to complete internally
        let axis = config.cnc_axis.clone();
        let cnc = self.cnc.clone();
        self.execute_with_reconnect(
            move || {
                let cnc = cnc.clone();
                let axis = axis.clone();
                async move {
                    let cnc_read = cnc.read().await;
                    cnc_read.home_axis(&axis).await
                }
            },
            "Home command",
        )
        .await?;

        // grblHAL automatically backs off from the limit switch after homing
        // Configure the pulloff distance with grblHAL setting $27 (homing pulloff in mm)
        // Example: $27=3.0 will back off 3mm from the limit switch
        tracing::info!("Homing complete, grblHAL pulloff handled by controller");

        // Reset position to zero (this is now our closed position)
        let reset_cmd = format!("G92 {}0", config.cnc_axis);
        let cnc = self.cnc.clone();
        self.execute_with_reconnect(
            move || {
                let cnc = cnc.clone();
                let reset_cmd = reset_cmd.clone();
                async move {
                    let cnc_read = cnc.read().await;
                    cnc_read.send_command(&reset_cmd).await
                }
            },
            "Reset position",
        )
        .await?;

        // Query current position and record as home position (use raw MPos, not relative)
        let cnc = self.cnc.read().await;
        if let Ok(status_str) = cnc.get_status().await {
            if let Ok(mpos) = self.parse_position(&status_str, false).await {
                let mut home_pos = self.home_position.lock().await;
                *home_pos = mpos;
                tracing::info!("Recorded home position: MPos = {}", mpos);
            } else {
                tracing::error!("Failed to parse position from status after G92: {}", status_str);
            }
        } else {
            tracing::error!("Failed to query status after G92");
        }
        drop(cnc);

        // Mark as homed and update status
        {
            let mut is_homed = self.is_homed.lock().await;
            *is_homed = true;
        }

        let updated_status = {
            let mut status = self.status.lock().await;
            status.position_mm = 0.0;
            status.position_percent = 0.0;
            status.state = DoorState::Closed;
            status.clone()
        };

        // Broadcast status update to all clients
        let _ = self.status_tx.send(updated_status);

        tracing::info!("Home operation complete - door is now at closed position");
        Ok(())
    }

    /// Zero the door (set current position as home without homing sequence)
    pub async fn zero(&self) -> Result<()> {
        // Always clear alarm before zeroing (soft reset + $X)
        // This ensures we can zero even if an alarm occurred but wasn't detected
        tracing::info!("Clearing any potential alarms before zeroing");
        self.clear_alarm().await?;

        tracing::info!("Zeroing door at current position");

        // Reset position to zero (set current position as home)
        let config = self.config.read().await;
        let reset_cmd = format!("G92 {}0", config.cnc_axis);
        drop(config);

        // Send reset command with automatic reconnection on connection errors
        let cnc = self.cnc.clone();
        self.execute_with_reconnect(
            move || {
                let cnc = cnc.clone();
                let reset_cmd = reset_cmd.clone();
                async move {
                    let cnc_read = cnc.read().await;
                    cnc_read.send_command(&reset_cmd).await
                }
            },
            "Zero command",
        )
        .await?;

        // Query current position and record as home position (use raw MPos, not relative)
        let cnc = self.cnc.read().await;
        if let Ok(status_str) = cnc.get_status().await {
            if let Ok(mpos) = self.parse_position(&status_str, false).await {
                let mut home_pos = self.home_position.lock().await;
                *home_pos = mpos;
                tracing::info!("Recorded home position: MPos = {}", mpos);
            } else {
                tracing::error!("Failed to parse position from status after G92: {}", status_str);
            }
        } else {
            tracing::error!("Failed to query status after G92");
        }
        drop(cnc);

        // Mark as homed and update status
        {
            let mut is_homed = self.is_homed.lock().await;
            *is_homed = true;
        }

        let updated_status = {
            let mut status = self.status.lock().await;
            status.position_mm = 0.0;
            status.position_percent = 0.0;
            status.state = DoorState::Closed;
            status.clone()
        };

        // Broadcast status update to all clients
        let _ = self.status_tx.send(updated_status);

        tracing::info!("Zero operation complete - current position set as home (closed)");
        Ok(())
    }

    /// Clear alarm state
    pub async fn clear_alarm(&self) -> Result<()> {
        let current_state = {
            let status = self.status.lock().await;
            status.state.clone()
        };

        // Log current state
        if current_state == DoorState::Alarm {
            tracing::info!("Clear alarm requested - system is in alarm state");
        } else {
            tracing::info!("Clear alarm requested - system is in {:?} state (will attempt clear anyway)", current_state);
        }

        // Step 1: Send soft reset (0x18 / Ctrl-X) to reset controller state
        tracing::info!("Sending soft reset (0x18) to CNC controller");
        let cnc = self.cnc.clone();
        self.execute_with_reconnect(
            move || {
                let cnc = cnc.clone();
                async move {
                    let cnc_read = cnc.read().await;
                    cnc_read.send_realtime_command(0x18).await
                }
            },
            "Soft reset before alarm clear",
        )
        .await
        .context("Failed to send soft reset")?;

        // Wait for controller to process reset
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        // Step 2: Send unlock command ($X) to clear the alarm
        tracing::info!("Sending unlock command ($X) to CNC controller");
        let cnc = self.cnc.clone();
        let result = self.execute_with_reconnect(
            move || {
                let cnc = cnc.clone();
                async move {
                    let cnc_read = cnc.read().await;
                    cnc_read.send_command("$X").await
                }
            },
            "Clear alarm",
        )
        .await;

        match result {
            Ok(_) => {
                tracing::info!("Clear alarm sequence completed successfully");

                // Query status to verify alarm was cleared
                tracing::debug!("Querying status after alarm clear to update clients");
                let cnc = self.cnc.read().await;
                if let Ok(status_str) = cnc.get_status().await {
                    drop(cnc);

                    // Check if alarm is still present
                    let (is_alarm, alarm_code) = CncController::parse_alarm(&status_str);

                    if is_alarm {
                        tracing::warn!("Alarm still present after clear attempt: {:?}", alarm_code);
                        let mut st = self.status.lock().await;
                        st.state = DoorState::Alarm;
                        st.alarm_code = alarm_code;
                        let updated_status = st.clone();
                        drop(st);
                        let _ = self.status_tx.send(updated_status);
                    } else {
                        tracing::info!("Alarm successfully cleared, resetting to pending state");

                        // Reset homed flag - soft reset loses position reference
                        {
                            let mut is_homed = self.is_homed.lock().await;
                            *is_homed = false;
                        }

                        // Update status to pending state
                        {
                            let mut st = self.status.lock().await;
                            st.state = DoorState::Pending;
                            st.alarm_code = None;
                            st.position_mm = 0.0;
                            st.position_percent = 0.0;
                            let updated_status = st.clone();
                            drop(st);
                            let _ = self.status_tx.send(updated_status);
                        }

                        tracing::info!("System reset to pending state - homing required");
                    }
                } else {
                    drop(cnc);
                    tracing::warn!("Failed to query status after alarm clear");
                }

                Ok(())
            }
            Err(e) => {
                tracing::error!("Clear alarm sequence failed: {}", e);
                Err(e).context("Failed to clear alarm")
            }
        }
    }

    /// Open the door
    pub async fn open(&self) -> Result<()> {
        {
            let status = self.status.lock().await;

            // Check if homed
            let is_homed = *self.is_homed.lock().await;
            if !is_homed {
                return Err(anyhow::anyhow!(
                    "Door must be homed before opening. Please run home command first."
                ));
            }

            // Only allow opening when door is Closed, Closing, or Intermediate
            match status.state {
                DoorState::Closed | DoorState::Intermediate => {
                    // Allow operation to proceed
                }
                DoorState::Closing => {
                    // Currently closing - stop it first then continue with open
                    drop(status);
                    tracing::info!("Door is closing, stopping and reversing to open");
                    self.stop().await?;
                }
                DoorState::Open => {
                    return Err(anyhow::anyhow!("Door is already open"));
                }
                DoorState::Opening => {
                    return Err(anyhow::anyhow!("Door is already opening"));
                }
                DoorState::Homing => {
                    return Err(anyhow::anyhow!("Door is currently homing. Wait for homing to complete."));
                }
                DoorState::Halting => {
                    return Err(anyhow::anyhow!("Door is currently halting. Wait for halt to complete."));
                }
                DoorState::Pending => {
                    return Err(anyhow::anyhow!("Door is in pending state. Home the door first."));
                }
                DoorState::Fault => {
                    return Err(anyhow::anyhow!(
                        "System is in fault state: {}",
                        status.fault_message.as_ref().unwrap_or(&"Unknown error".to_string())
                    ));
                }
                DoorState::Alarm => {
                    let alarm_msg = if let Some(code) = &status.alarm_code {
                        format!("CNC is in alarm state (Code {}). Use clear_alarm command first.", code)
                    } else {
                        "CNC is in alarm state. Use clear_alarm command first.".to_string()
                    };
                    return Err(anyhow::anyhow!(alarm_msg));
                }
            }
        }

        let config = self.config.read().await;
        let open_distance = config.open_distance;
        let open_speed = config.open_speed;
        let axis = config.cnc_axis.clone();

        // Calculate target position based on direction
        let target_position = if config.open_direction.to_lowercase() == "left" {
            -open_distance
        } else {
            open_distance
        };
        drop(config);

        tracing::info!("Opening door to {} mm at {} mm/min", target_position, open_speed);

        // Send move command with automatic reconnection on connection errors
        let cnc = self.cnc.clone();
        self.execute_with_reconnect(
            move || {
                let cnc = cnc.clone();
                let axis = axis.clone();
                async move {
                    let cnc_read = cnc.read().await;
                    cnc_read.move_absolute(&axis, target_position, open_speed).await
                }
            },
            "Open command",
        )
        .await?;

        // Set state to opening AFTER sending command to avoid race condition
        {
            let mut discard = self.discard_next_poll.lock().await;
            *discard = true;
            drop(discard);

            let mut status = self.status.lock().await;
            status.state = DoorState::Opening;
        }

        Ok(())
    }

    /// Close the door
    pub async fn close(&self) -> Result<()> {
        {
            let status = self.status.lock().await;

            // Check if homed
            let is_homed = *self.is_homed.lock().await;
            if !is_homed {
                return Err(anyhow::anyhow!(
                    "Door must be homed before closing. Please run home command first."
                ));
            }

            // Only allow closing when door is Open, Opening, or Intermediate
            match status.state {
                DoorState::Open | DoorState::Intermediate => {
                    // Allow operation to proceed
                }
                DoorState::Opening => {
                    // Currently opening - stop it first then continue with close
                    drop(status);
                    tracing::info!("Door is opening, stopping and reversing to close");
                    self.stop().await?;
                }
                DoorState::Closed => {
                    return Err(anyhow::anyhow!("Door is already closed"));
                }
                DoorState::Closing => {
                    return Err(anyhow::anyhow!("Door is already closing"));
                }
                DoorState::Homing => {
                    return Err(anyhow::anyhow!("Door is currently homing. Wait for homing to complete."));
                }
                DoorState::Halting => {
                    return Err(anyhow::anyhow!("Door is currently halting. Wait for halt to complete."));
                }
                DoorState::Pending => {
                    return Err(anyhow::anyhow!("Door is in pending state. Home the door first."));
                }
                DoorState::Fault => {
                    return Err(anyhow::anyhow!(
                        "System is in fault state: {}",
                        status.fault_message.as_ref().unwrap_or(&"Unknown error".to_string())
                    ));
                }
                DoorState::Alarm => {
                    let alarm_msg = if let Some(code) = &status.alarm_code {
                        format!("CNC is in alarm state (Code {}). Use clear_alarm command first.", code)
                    } else {
                        "CNC is in alarm state. Use clear_alarm command first.".to_string()
                    };
                    return Err(anyhow::anyhow!(alarm_msg));
                }
            }
        }

        let config = self.config.read().await;
        let close_speed = config.close_speed;
        let axis = config.cnc_axis.clone();
        drop(config);

        tracing::info!("Closing door to 0 mm at {} mm/min", close_speed);

        // Send move command to home position (0mm) with automatic reconnection on connection errors
        let cnc = self.cnc.clone();
        self.execute_with_reconnect(
            move || {
                let cnc = cnc.clone();
                let axis = axis.clone();
                async move {
                    let cnc_read = cnc.read().await;
                    cnc_read.move_absolute(&axis, 0.0, close_speed).await
                }
            },
            "Close command",
        )
        .await?;

        // Set state to closing AFTER sending command to avoid race condition
        {
            let mut discard = self.discard_next_poll.lock().await;
            *discard = true;
            drop(discard);

            let mut status = self.status.lock().await;
            status.state = DoorState::Closing;
        }

        Ok(())
    }

    /// Jog the door by a relative distance in mm
    pub async fn jog(&self, distance: f64, feed_rate: Option<f64>) -> Result<()> {
        {
            let status = self.status.lock().await;

            // Check state - don't allow jogging during certain states
            match status.state {
                DoorState::Opening | DoorState::Closing | DoorState::Homing | DoorState::Halting => {
                    return Err(anyhow::anyhow!("Cannot jog while door is moving (state: {:?})", status.state));
                }
                DoorState::Fault => {
                    return Err(anyhow::anyhow!(
                        "System is in fault state: {}",
                        status.fault_message.as_ref().unwrap_or(&"Unknown error".to_string())
                    ));
                }
                DoorState::Alarm => {
                    let alarm_msg = if let Some(code) = &status.alarm_code {
                        format!("CNC is in alarm state (Code {}). Use clear_alarm command first.", code)
                    } else {
                        "CNC is in alarm state. Use clear_alarm command first.".to_string()
                    };
                    return Err(anyhow::anyhow!(alarm_msg));
                }
                _ => {} // Allow jogging in any non-moving state (including when not homed)
            }
        }

        let config = self.config.read().await;
        let axis = config.cnc_axis.clone();

        // Use provided feed rate or default to open_speed
        let jog_feed_rate = feed_rate.unwrap_or(config.open_speed);

        // Calculate jog distance based on direction
        let jog_distance = if config.open_direction.to_lowercase() == "left" {
            -distance
        } else {
            distance
        };
        drop(config);

        tracing::info!("Jogging {} mm at {} mm/min", jog_distance, jog_feed_rate);

        // Send jog command with automatic reconnection on connection errors
        let cnc = self.cnc.clone();
        self.execute_with_reconnect(
            move || {
                let cnc = cnc.clone();
                let axis = axis.clone();
                async move {
                    let cnc_read = cnc.read().await;
                    cnc_read.jog(&axis, jog_distance, jog_feed_rate).await
                }
            },
            "Jog command",
        )
        .await?;

        Ok(())
    }

    /// Move to a specific percentage (0-100)
    pub async fn move_to_percent(&self, percent: f64) -> Result<()> {
        // Validate percentage
        if percent < 0.0 || percent > 100.0 {
            return Err(anyhow::anyhow!("Percentage must be between 0 and 100, got {}", percent));
        }

        {
            let status = self.status.lock().await;

            // Check if homed
            let is_homed = *self.is_homed.lock().await;
            if !is_homed {
                return Err(anyhow::anyhow!("Door must be homed before moving. Please run home command first."));
            }

            // Check if already moving - if so, ignore this command
            match status.state {
                DoorState::Opening | DoorState::Closing | DoorState::Homing | DoorState::Halting => {
                    return Err(anyhow::anyhow!("Door is already moving (state: {:?}). Wait for current operation to complete.", status.state));
                }
                DoorState::Fault => {
                    return Err(anyhow::anyhow!(
                        "System is in fault state: {}",
                        status.fault_message.as_ref().unwrap_or(&"Unknown error".to_string())
                    ));
                }
                DoorState::Alarm => {
                    let alarm_msg = if let Some(code) = &status.alarm_code {
                        format!("CNC is in alarm state (Code {}). Use clear_alarm command first.", code)
                    } else {
                        "CNC is in alarm state. Use clear_alarm command first.".to_string()
                    };
                    return Err(anyhow::anyhow!(alarm_msg));
                }
                _ => {} // Closed, Open, Intermediate, Pending - allow movement
            }
        }

        let config = self.config.read().await;
        let open_speed = config.open_speed;
        let close_speed = config.close_speed;
        let axis = config.cnc_axis.clone();

        // Calculate target position
        let target_position = if config.open_direction.to_lowercase() == "left" {
            -(config.open_distance * percent / 100.0)
        } else {
            config.open_distance * percent / 100.0
        };

        // Get current position to determine direction
        let current_pos = self.status.lock().await.position_mm;

        // Determine if opening or closing
        let moving_toward_open = target_position.abs() > current_pos.abs();
        let speed = if moving_toward_open { open_speed } else { close_speed };
        let new_state = if moving_toward_open { DoorState::Opening } else { DoorState::Closing };

        tracing::info!("Moving to {}% (position {} mm) at {} mm/min", percent, target_position, speed);

        // Send move command
        let cnc = self.cnc.clone();
        self.execute_with_reconnect(
            move || {
                let cnc = cnc.clone();
                let axis = axis.clone();
                async move {
                    let cnc_read = cnc.read().await;
                    cnc_read.move_absolute(&axis, target_position, speed).await
                }
            },
            "Move to percent",
        )
        .await?;

        // Set state AFTER sending command to avoid race condition
        {
            let mut discard = self.discard_next_poll.lock().await;
            *discard = true;
            drop(discard);

            let mut status = self.status.lock().await;
            status.state = new_state;
        }

        Ok(())
    }

    /// Stop mid-movement.
    ///
    /// This method safely decelerates the door to a stop using feed hold, 
    /// then flushes the command queue to clear any pending actions.
    ///
    /// Blocking call.
    pub async fn stop(&self) -> Result<()> {
        // Set stop flag
        let mut stop_flag = self.stop_requested.lock().await;
        *stop_flag = true;
        drop(stop_flag);

        // Set state to halting
        {
            let mut status = self.status.lock().await;
            status.state = DoorState::Halting;
        }


        // Step 1: Send feed hold to decelerate safely
        // Feed hold (!) respects $120 acceleration settings and decelerates properly
        tracing::info!("Stop requested - sending feed hold");
        let cnc = self.cnc.clone();
        self.execute_with_reconnect(
            move || {
                let cnc = cnc.clone();
                async move {
                    let cnc_read = cnc.read().await;
                    cnc_read.feed_hold().await
                }
            },
            "Feed hold",
        )
        .await?;

        // Step 2: Poll status until we see "Hold:0" (motor fully stopped)
        // Hold:1 means still stopping, Hold:0 means stopped
        tracing::info!("Polling status until motor stops (Hold:0)");
        let mut attempts = 0;
        const MAX_ATTEMPTS: u32 = 50; // 5 seconds max wait (100ms * 50)
        
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            attempts += 1;

            if attempts > MAX_ATTEMPTS {
                tracing::warn!("Timeout waiting for Hold:0 state, proceeding with queue flush");
                break;
            }

            let cnc = self.cnc.read().await;
            if let Ok(status_str) = cnc.get_status().await {
                drop(cnc);
                
                if let Ok(state) = CncController::parse_state(&status_str) {
                    tracing::debug!("Current state: {}", state);
                    
                    // Check for Hold:0 (fully stopped)
                    if state == "Hold:0" {
                        tracing::info!("Motor stopped (Hold:0)");
                        break;
                    }
                    // If we're already in Idle, we're done
                    else if state == "Idle" {
                        tracing::info!("Motor already in Idle state");
                        break;
                    }
                    // Hold:1 means still stopping, continue polling
                    else if state == "Hold:1" {
                        tracing::debug!("Motor still decelerating (Hold:1)");
                    }
                }
            } else {
                drop(cnc);
            }
        }

        // Step 3: Send queue flush to clear pending commands gracefully
        // Motor is already stopped, so this is safe (no sudden deceleration)
        tracing::info!("Sending queue flush to clear pending commands");
        let cnc = self.cnc.clone();
        self.execute_with_reconnect(
            move || {
                let cnc = cnc.clone();
                async move {
                    let cnc_read = cnc.read().await;
                    cnc_read.queue_flush().await
                }
            },
            "Queue flush",
        )
        .await?;

        // Verify position is still tracked after reset (with timeout to prevent hanging)
        let status_query = async {
            let cnc = self.cnc.read().await;
            cnc.get_status().await
        };

        match tokio::time::timeout(Duration::from_secs(3), status_query).await {
            Ok(Ok(status_str)) => {
                let config = self.config.read().await;
                let homed = *self.is_homed.lock().await;
                let relative_pos = self.parse_position(&status_str, true).await.unwrap_or(0.0);

                let mut status = self.status.lock().await;
                status.position_mm = relative_pos;
                status.position_percent = Self::calculate_position_percent(relative_pos, config.open_distance);

                // Determine state based on position
                if homed {
                    // Calculate target open position based on direction
                    let target_open_pos = if config.open_direction.to_lowercase() == "left" {
                        -config.open_distance
                    } else {
                        config.open_distance
                    };

                    // Check if at closed position (within 0.1mm for floating point precision)
                    if relative_pos.abs() < 0.1 {
                        status.state = DoorState::Closed;
                    }
                    // Check if at open position (within 0.1mm for floating point precision)
                    else if (relative_pos - target_open_pos).abs() < 0.1 {
                        status.state = DoorState::Open;
                    }
                    // At intermediate position - we're stopped but not at a defined position
                    else {
                        status.state = DoorState::Intermediate;
                    }
                } else {
                    status.state = DoorState::Pending;
                }

                tracing::info!("Stop complete, position verified at {} mm (relative to home)", relative_pos);
            }
            Ok(Err(e)) => {
                // Status query failed
                tracing::warn!("Stop complete, but status query failed: {}", e);
                let homed = *self.is_homed.lock().await;
                let mut status = self.status.lock().await;
                status.state = if homed { DoorState::Intermediate } else { DoorState::Pending };
            }
            Err(_) => {
                // Status query timed out
                tracing::error!("Stop complete, but status query timed out after 3 seconds");
                let homed = *self.is_homed.lock().await;
                let mut status = self.status.lock().await;
                status.state = if homed { DoorState::Intermediate } else { DoorState::Pending };
            }
        }

        // Clear stop flag
        let mut stop_flag = self.stop_requested.lock().await;
        *stop_flag = false;

        Ok(())
    }

    /// Wait for CNC to reach idle state
    /// Uses longer polling intervals to avoid flooding the serial buffer during
    /// operations like homing where the controller doesn't respond to queries
    async fn wait_for_idle(&self) -> Result<()> {
        let mut attempts = 0;
        const MAX_ATTEMPTS: u32 = 60; // 60 seconds max wait
        const POLL_INTERVAL_MS: u64 = 1000; // Poll every 1 second

        loop {
            tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
            attempts += 1;

            if attempts > MAX_ATTEMPTS {
                return Err(anyhow::anyhow!("Timeout waiting for CNC to reach idle"));
            }

            let cnc = self.cnc.read().await;
            if let Ok(status_str) = cnc.get_status().await {
                if let Ok(state) = CncController::parse_state(&status_str) {
                    if state == "Idle" {
                        return Ok(());
                    }
                    // Log current state if not idle
                    tracing::debug!("Waiting for idle, current state: {}", state);
                }
            }
            // If query times out or fails, just continue waiting
            // (grblHAL doesn't respond to status queries during some operations like homing)
        }
    }

    /// Query all CNC settings
    pub async fn query_cnc_settings(&self) -> Result<indexmap::IndexMap<String, String>> {
        let cnc = self.cnc.read().await;
        cnc.query_settings().await
    }

    /// Get a specific CNC setting
    pub async fn get_cnc_setting(&self, setting_name: &str) -> Result<String> {
        let cnc = self.cnc.read().await;
        cnc.get_setting(setting_name).await
    }

    /// Set a specific CNC setting
    pub async fn set_cnc_setting(&self, setting_name: &str, value: &str) -> Result<()> {
        let cnc = self.cnc.read().await;
        cnc.set_setting(setting_name, value).await
    }
}

impl Clone for DoorController {
    fn clone(&self) -> Self {
        Self {
            cnc: self.cnc.clone(),
            config: self.config.clone(),
            status: self.status.clone(),
            is_homed: self.is_homed.clone(),
            home_position: self.home_position.clone(),
            stop_requested: self.stop_requested.clone(),
            auto_home_done: self.auto_home_done.clone(),
            discard_next_poll: self.discard_next_poll.clone(),
            status_tx: self.status_tx.clone(),
        }
    }
}
