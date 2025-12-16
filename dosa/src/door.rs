use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
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
}

impl DoorController {
    /// Create a new door controller
    pub async fn new(cnc: CncController, config: DoorConfig) -> Result<Self> {
        let controller = Self {
            cnc: Arc::new(RwLock::new(Arc::new(cnc))),
            config: Arc::new(RwLock::new(config)),
            status: Arc::new(Mutex::new(DoorStatus {
                state: DoorState::Pending,
                position_mm: 0.0,
                fault_message: None,
                alarm_code: None,
            })),
            is_homed: Arc::new(Mutex::new(false)),
            home_position: Arc::new(Mutex::new(0.0)),
            stop_requested: Arc::new(Mutex::new(false)),
        };

        // Start background position monitoring
        controller.start_position_monitor();

        Ok(controller)
    }

    /// Create a door controller in fault state (when initialization fails)
    pub fn new_fault(error: String, config: DoorConfig) -> Self {
        let controller = Self {
            cnc: Arc::new(RwLock::new(Arc::new(CncController::dummy()))),
            config: Arc::new(RwLock::new(config)),
            status: Arc::new(Mutex::new(DoorStatus {
                state: DoorState::Fault,
                position_mm: 0.0,
                fault_message: Some(error),
                alarm_code: None,
            })),
            is_homed: Arc::new(Mutex::new(false)),
            home_position: Arc::new(Mutex::new(0.0)),
            stop_requested: Arc::new(Mutex::new(false)),
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

        // Get current config
        let config = self.config.read().await.clone();

        // Try to create new CNC connection
        let cnc = CncController::new(&config.cnc_connection)
            .await
            .context("Failed to create new CNC connection")?;

        // Validate stop_delay_ms
        cnc.validate_stop_delay(
            &config.cnc_axis,
            config.open_speed,
            config.close_speed,
            config.stop_delay_ms,
        )
        .await
        .context("Stop delay validation failed")?;

        // Reconnect
        self.reconnect(cnc, config).await?;

        tracing::info!("Reconnection successful");
        Ok(())
    }

    /// Start background task to monitor position
    fn start_position_monitor(&self) {
        let cnc = self.cnc.clone();
        let config = self.config.clone();
        let status = self.status.clone();
        let is_homed = self.is_homed.clone();
        let home_position = self.home_position.clone();

        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_millis(200));

            loop {
                ticker.tick().await;

                // Only poll during active movement (Opening, Closing, Homing)
                // Skip polling when in other states
                {
                    let st = status.lock().await;

                    // Only monitor during movement operations
                    match st.state {
                        DoorState::Opening | DoorState::Closing | DoorState::Homing => {
                            // Continue to poll
                        }
                        _ => {
                            // Don't poll when not moving
                            continue;
                        }
                    }
                }

                // Query CNC status during movement
                let cnc_read = cnc.read().await;
                if let Ok(status_str) = cnc_read.get_status().await {
                    let cfg = config.read().await;
                    let homed = *is_homed.lock().await;
                    let mut st = status.lock().await;

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
                            tracing::debug!("[Monitor] Position: MPos={}, HomePos={}, Relative={}", mpos, home_pos, st.position_mm);
                        } else {
                            st.position_mm = 0.0;
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
                                    // Otherwise door is between positions - keep previous state or set to closed if transitioning
                                    else if prev_state == DoorState::Opening || prev_state == DoorState::Closing {
                                        // Movement stopped mid-travel
                                        st.state = DoorState::Intermediate;
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
                }
            }
        });
    }

    /// Get current door status (returns cached status)
    pub async fn get_status(&self) -> DoorStatus {
        self.status.lock().await.clone()
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
        // Query CNC controller (even if in fault state, to allow recovery)
        let status_result = {
            let cnc = self.cnc.read().await;
            cnc.get_status().await
        }; // Drop read lock before processing result

        match status_result {
            Ok(status_str) => {
                let cfg = self.config.read().await;
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
                let mut st = self.status.lock().await;
                st.position_mm = position;

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
                                if (pos - 0.0).abs() < 0.1 {
                                    st.state = DoorState::Closed;
                                }
                                // Check if at open position (within 0.1mm for floating point precision)
                                else if (pos - target_open_pos).abs() < 0.1 {
                                    st.state = DoorState::Open;
                                }
                                // Otherwise keep current state
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
            Err(e) => {
                // Connection error - attempt quick reconnection with tight timeout
                tracing::warn!("Status query failed: {}. Attempting reconnection...", e);

                // Wrap entire reconnection + retry in 3-second timeout to avoid blocking WebSocket
                let reconnect_and_retry = async {
                    // Try to reconnect
                    self.try_reconnect().await?;

                    // Reconnection succeeded - retry the query
                    let cnc = self.cnc.read().await;
                    let status_str = cnc.get_status().await?;
                    let position = self.parse_position(&status_str, true).await.unwrap_or(0.0);

                    let mut st = self.status.lock().await;
                    st.position_mm = position;
                    st.state = DoorState::Pending; // After reconnect, needs re-homing

                    Ok::<DoorStatus, anyhow::Error>(st.clone())
                };

                match tokio::time::timeout(Duration::from_secs(3), reconnect_and_retry).await {
                    Ok(Ok(status)) => {
                        tracing::info!("Reconnection successful");
                        Ok(status)
                    }
                    Ok(Err(reconnect_err)) => {
                        tracing::error!("Reconnection failed: {}", reconnect_err);
                        let mut st = self.status.lock().await;
                        st.state = DoorState::Fault;
                        st.fault_message = Some(format!("Connection lost: {}", reconnect_err));
                        Ok(st.clone())
                    }
                    Err(_) => {
                        tracing::error!("Reconnection timed out after 3 seconds");
                        let mut st = self.status.lock().await;
                        st.state = DoorState::Fault;
                        st.fault_message = Some("Reconnection timed out".to_string());
                        Ok(st.clone())
                    }
                }
            }
        }
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

            // Check if in fault state
            if status.state == DoorState::Fault {
                return Err(anyhow::anyhow!(
                    "System is in fault state: {}",
                    status.fault_message.as_ref().unwrap_or(&"Unknown error".to_string())
                ));
            }

            // Check if in alarm state
            if status.state == DoorState::Alarm {
                let alarm_msg = if let Some(code) = &status.alarm_code {
                    format!("CNC is in alarm state (Code {}). Use clear_alarm command first.", code)
                } else {
                    "CNC is in alarm state. Use clear_alarm command first.".to_string()
                };
                return Err(anyhow::anyhow!(alarm_msg));
            }

            // Check if already homing
            if status.state == DoorState::Homing {
                return Ok(());
            }
        }

        let config = self.config.read().await;

        // Set state to homing
        {
            let mut status = self.status.lock().await;
            status.state = DoorState::Homing;
        }

        tracing::info!("Homing door on {} axis", config.cnc_axis);

        // Send home command with reconnection on failure
        let cnc = self.cnc.read().await;
        let result = cnc.home_axis(&config.cnc_axis).await;
        drop(cnc);

        if let Err(e) = result {
            tracing::warn!("Home command failed: {}. Attempting reconnection...", e);
            if let Err(reconnect_err) = self.try_reconnect().await {
                self.set_fault(format!("Failed to reconnect: {}", reconnect_err)).await;
                return Err(anyhow::anyhow!("Home failed and reconnection failed: {}", reconnect_err));
            }

            // Retry after successful reconnection
            let cnc = self.cnc.read().await;
            cnc.home_axis(&config.cnc_axis)
                .await
                .context("Home failed after reconnection")?;
        }

        // Wait for homing to complete
        self.wait_for_idle().await?;

        // Move back by limit offset
        tracing::info!("Moving back by {} mm from limit", config.limit_offset);
        let cnc = self.cnc.read().await;
        let result = cnc.move_absolute(&config.cnc_axis, config.limit_offset, config.close_speed).await;
        drop(cnc);

        if let Err(e) = result {
            tracing::warn!("Move to offset failed: {}. Attempting reconnection...", e);
            if let Err(reconnect_err) = self.try_reconnect().await {
                self.set_fault(format!("Failed to reconnect: {}", reconnect_err)).await;
                return Err(anyhow::anyhow!("Move failed and reconnection failed: {}", reconnect_err));
            }

            let cnc = self.cnc.read().await;
            cnc.move_absolute(&config.cnc_axis, config.limit_offset, config.close_speed)
                .await
                .context("Move to offset failed after reconnection")?;
        }

        // Wait for move to complete
        self.wait_for_idle().await?;

        // Reset position to zero (this is now our closed position)
        let reset_cmd = format!("G92 {}0", config.cnc_axis);
        let cnc = self.cnc.read().await;
        let result = cnc.send_command(&reset_cmd).await;
        drop(cnc);

        if let Err(e) = result {
            tracing::warn!("Reset position failed: {}. Attempting reconnection...", e);
            if let Err(reconnect_err) = self.try_reconnect().await {
                self.set_fault(format!("Failed to reconnect: {}", reconnect_err)).await;
                return Err(anyhow::anyhow!("Reset position failed and reconnection failed: {}", reconnect_err));
            }

            let cnc = self.cnc.read().await;
            cnc.send_command(&reset_cmd)
                .await
                .context("Reset position failed after reconnection")?;
        }

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

        {
            let mut status = self.status.lock().await;
            status.position_mm = 0.0;
            status.state = DoorState::Closed;
        }

        tracing::info!("Home operation complete - door is now at closed position");
        Ok(())
    }

    /// Zero the door (set current position as home without homing sequence)
    pub async fn zero(&self) -> Result<()> {
        {
            let status = self.status.lock().await;

            // Check if in fault state
            if status.state == DoorState::Fault {
                return Err(anyhow::anyhow!(
                    "System is in fault state: {}",
                    status.fault_message.as_ref().unwrap_or(&"Unknown error".to_string())
                ));
            }

            // Check if in alarm state
            if status.state == DoorState::Alarm {
                let alarm_msg = if let Some(code) = &status.alarm_code {
                    format!("CNC is in alarm state (Code {}). Use clear_alarm command first.", code)
                } else {
                    "CNC is in alarm state. Use clear_alarm command first.".to_string()
                };
                return Err(anyhow::anyhow!(alarm_msg));
            }
        }

        tracing::info!("Zeroing door at current position");

        // Reset position to zero (set current position as home)
        let config = self.config.read().await;
        let reset_cmd = format!("G92 {}0", config.cnc_axis);
        drop(config);

        let cnc = self.cnc.read().await;
        let result = cnc.send_command(&reset_cmd).await;
        drop(cnc);

        if let Err(e) = result {
            tracing::warn!("Zero command failed: {}. Attempting reconnection...", e);
            if let Err(reconnect_err) = self.try_reconnect().await {
                self.set_fault(format!("Failed to reconnect: {}", reconnect_err)).await;
                return Err(anyhow::anyhow!("Zero failed and reconnection failed: {}", reconnect_err));
            }

            let cnc = self.cnc.read().await;
            cnc.send_command(&reset_cmd)
                .await
                .context("Zero failed after reconnection")?;
        }

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

        {
            let mut status = self.status.lock().await;
            status.position_mm = 0.0;
            status.state = DoorState::Closed;
        }

        tracing::info!("Zero operation complete - current position set as home (closed)");
        Ok(())
    }

    /// Clear alarm state
    pub async fn clear_alarm(&self) -> Result<()> {
        {
            let status = self.status.lock().await;

            // Check if actually in alarm state
            if status.state != DoorState::Alarm {
                return Ok(());
            }
        }

        tracing::info!("Clearing CNC alarm");

        // Send unlock command to grblHAL ($X)
        let cnc = self.cnc.read().await;
        let result = cnc.send_command("$X").await;
        drop(cnc);

        if let Err(e) = result {
            tracing::warn!("Clear alarm command failed: {}. Attempting reconnection...", e);
            if let Err(reconnect_err) = self.try_reconnect().await {
                self.set_fault(format!("Failed to reconnect: {}", reconnect_err)).await;
                return Err(anyhow::anyhow!("Clear alarm failed and reconnection failed: {}", reconnect_err));
            }

            let cnc = self.cnc.read().await;
            cnc.send_command("$X")
                .await
                .context("Clear alarm failed after reconnection")?;
        }

        tracing::info!("Alarm clear command sent - monitor will update status");
        Ok(())
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

        // Set state to opening
        {
            let mut status = self.status.lock().await;
            status.state = DoorState::Opening;
        }

        tracing::info!("Opening door to {} mm at {} mm/min", target_position, open_speed);

        // Send move command with reconnection on failure
        let cnc = self.cnc.read().await;
        let result = cnc.move_absolute(&axis, target_position, open_speed).await;
        drop(cnc);

        if let Err(e) = result {
            tracing::warn!("Open command failed: {}. Attempting reconnection...", e);
            if let Err(reconnect_err) = self.try_reconnect().await {
                self.set_fault(format!("Failed to reconnect: {}", reconnect_err)).await;
                return Err(anyhow::anyhow!("Open failed and reconnection failed: {}", reconnect_err));
            }

            let cnc = self.cnc.read().await;
            cnc.move_absolute(&axis, target_position, open_speed)
                .await
                .context("Open failed after reconnection")?;
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

        // Set state to closing
        {
            let mut status = self.status.lock().await;
            status.state = DoorState::Closing;
        }

        tracing::info!("Closing door to 0 mm at {} mm/min", close_speed);

        // Send move command to home position (0mm) with reconnection on failure
        let cnc = self.cnc.read().await;
        let result = cnc.move_absolute(&axis, 0.0, close_speed).await;
        drop(cnc);

        if let Err(e) = result {
            tracing::warn!("Close command failed: {}. Attempting reconnection...", e);
            if let Err(reconnect_err) = self.try_reconnect().await {
                self.set_fault(format!("Failed to reconnect: {}", reconnect_err)).await;
                return Err(anyhow::anyhow!("Close failed and reconnection failed: {}", reconnect_err));
            }

            let cnc = self.cnc.read().await;
            cnc.move_absolute(&axis, 0.0, close_speed)
                .await
                .context("Close failed after reconnection")?;
        }

        Ok(())
    }

    /// Emergency stop
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
        let cnc = self.cnc.read().await;
        let result = cnc.feed_hold().await;
        drop(cnc);

        if let Err(e) = result {
            tracing::warn!("Feed hold command failed: {}. Attempting reconnection...", e);
            if let Err(reconnect_err) = self.try_reconnect().await {
                self.set_fault(format!("Failed to reconnect: {}", reconnect_err)).await;
                return Err(anyhow::anyhow!("Feed hold failed and reconnection failed: {}", reconnect_err));
            }

            let cnc = self.cnc.read().await;
            cnc.feed_hold()
                .await
                .context("Feed hold failed after reconnection")?;
        }

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
        let cnc = self.cnc.read().await;
        let result = cnc.queue_flush().await;
        drop(cnc);

        if let Err(e) = result {
            tracing::warn!("Queue flush command failed: {}. Attempting reconnection...", e);
            if let Err(reconnect_err) = self.try_reconnect().await {
                self.set_fault(format!("Failed to reconnect: {}", reconnect_err)).await;
                return Err(anyhow::anyhow!("Queue flush failed and reconnection failed: {}", reconnect_err));
            }

            let cnc = self.cnc.read().await;
            cnc.queue_flush()
                .await
                .context("Queue flush failed after reconnection")?;
        }

        // Verify position is still tracked after reset
        let cnc = self.cnc.read().await;
        if let Ok(status_str) = cnc.get_status().await {
            let config = self.config.read().await;
            let homed = *self.is_homed.lock().await;
            let relative_pos = self.parse_position(&status_str, true).await.unwrap_or(0.0);

            let mut status = self.status.lock().await;
            status.position_mm = relative_pos;

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
                // Clear any motion state (Closing/Opening) to prevent interrupt loops
                else if matches!(status.state, DoorState::Closing | DoorState::Opening) {
                    status.state = DoorState::Intermediate;
                }
            } else {
                status.state = DoorState::Pending;
            }

            tracing::info!("Stop complete, position verified at {} mm (relative to home)", relative_pos);
        } else {
            // Update status even if we can't verify position
            let homed = *self.is_homed.lock().await;
            let mut status = self.status.lock().await;
            status.state = if homed { DoorState::Closed } else { DoorState::Pending };
            tracing::warn!("Stop complete, but could not verify position");
        }

        // Clear stop flag
        let mut stop_flag = self.stop_requested.lock().await;
        *stop_flag = false;

        Ok(())
    }

    /// Wait for CNC to reach idle state
    async fn wait_for_idle(&self) -> Result<()> {
        let mut attempts = 0;
        const MAX_ATTEMPTS: u32 = 100; // 20 seconds max wait

        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
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
                }
            }
        }
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
        }
    }
}
