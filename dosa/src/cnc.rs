use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_serial::SerialPortBuilderExt;

use crate::config::CncConnection;

/// CNC controller client for grblHAL
pub struct CncController {
    connection: Arc<Mutex<CncConnectionType>>,
}

enum CncConnectionType {
    Tcp(BufReader<TcpStream>),
    Serial(BufReader<tokio_serial::SerialStream>),
    Dummy, // For fault state when CNC is not connected
}

impl CncController {
    /// Create a dummy CNC controller for fault state
    pub fn dummy() -> Self {
        Self {
            connection: Arc::new(Mutex::new(CncConnectionType::Dummy)),
        }
    }

    /// Create a new CNC controller connection
    pub async fn new(config: &CncConnection) -> Result<Self> {
        let connection = match config {
            CncConnection::Tcp { host, port } => {
                tracing::info!("Connecting to CNC controller at {}:{}", host, port);
                let stream = TcpStream::connect(format!("{}:{}", host, port))
                    .await
                    .context("Failed to connect to CNC controller via TCP")?;
                let reader = BufReader::new(stream);
                CncConnectionType::Tcp(reader)
            }
            CncConnection::Serial { port, baud_rate } => {
                tracing::info!(
                    "Connecting to CNC controller on serial port {} at {} baud",
                    port,
                    baud_rate
                );
                let serial = tokio_serial::new(port, *baud_rate)
                    .open_native_async()
                    .context("Failed to open serial port")?;
                let reader = BufReader::new(serial);
                CncConnectionType::Serial(reader)
            }
        };

        let controller = Self {
            connection: Arc::new(Mutex::new(connection)),
        };

        // Small delay to let connection stabilize
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        Ok(controller)
    }

    /// Query all grblHAL settings
    pub async fn query_settings(&self) -> Result<String> {
        let mut conn = self.connection.lock().await;

        let cmd = "$$\n";
        tracing::debug!("Sending CNC command: $$");

        match &mut *conn {
            CncConnectionType::Tcp(reader) => {
                let stream = reader.get_mut();
                stream
                    .write_all(cmd.as_bytes())
                    .await
                    .context("Failed to send settings query command to CNC")?;

                stream
                    .flush()
                    .await
                    .context("Failed to flush command to CNC")?;

                // Read all lines until we get "ok" with timeout
                let mut all_settings = String::new();
                let mut lines_read = 0;
                const MAX_LINES: usize = 200; // Safety limit
                const READ_TIMEOUT_MS: u64 = 2000; // 2 second timeout per line

                loop {
                    let mut line = String::new();
                    let read_result = tokio::time::timeout(
                        tokio::time::Duration::from_millis(READ_TIMEOUT_MS),
                        reader.read_line(&mut line)
                    ).await;

                    match read_result {
                        Ok(Ok(0)) => {
                            anyhow::bail!("Connection closed while reading settings (read {} lines)", lines_read);
                        }
                        Ok(Ok(_)) => {
                            let trimmed = line.trim();
                            tracing::trace!("Settings line {}: {}", lines_read, trimmed);

                            if trimmed == "ok" {
                                tracing::debug!("Received 'ok', settings complete ({} lines)", lines_read);
                                break;
                            }

                            if !trimmed.is_empty() {
                                all_settings.push_str(&line);
                                lines_read += 1;
                            }

                            if lines_read >= MAX_LINES {
                                anyhow::bail!("Too many lines reading settings (safety limit)");
                            }
                        }
                        Ok(Err(e)) => {
                            return Err(e).context(format!("Failed to read settings from CNC (after {} lines)", lines_read));
                        }
                        Err(_) => {
                            anyhow::bail!("Timeout reading settings from CNC (after {} lines)", lines_read);
                        }
                    }
                }

                tracing::debug!("CNC settings response: {} lines", all_settings.lines().count());
                Ok(all_settings)
            }
            CncConnectionType::Serial(reader) => {
                let stream = reader.get_mut();
                stream
                    .write_all(cmd.as_bytes())
                    .await
                    .context("Failed to send settings query command to CNC")?;

                stream
                    .flush()
                    .await
                    .context("Failed to flush command to CNC")?;

                // Read all lines until we get "ok" with timeout
                let mut all_settings = String::new();
                let mut lines_read = 0;
                const MAX_LINES: usize = 200;
                const READ_TIMEOUT_MS: u64 = 2000;

                loop {
                    let mut line = String::new();
                    let read_result = tokio::time::timeout(
                        tokio::time::Duration::from_millis(READ_TIMEOUT_MS),
                        reader.read_line(&mut line)
                    ).await;

                    match read_result {
                        Ok(Ok(0)) => {
                            anyhow::bail!("Connection closed while reading settings (read {} lines)", lines_read);
                        }
                        Ok(Ok(_)) => {
                            let trimmed = line.trim();
                            tracing::trace!("Settings line {}: {}", lines_read, trimmed);

                            if trimmed == "ok" {
                                tracing::debug!("Received 'ok', settings complete ({} lines)", lines_read);
                                break;
                            }

                            if !trimmed.is_empty() {
                                all_settings.push_str(&line);
                                lines_read += 1;
                            }

                            if lines_read >= MAX_LINES {
                                anyhow::bail!("Too many lines reading settings (safety limit)");
                            }
                        }
                        Ok(Err(e)) => {
                            return Err(e).context(format!("Failed to read settings from CNC (after {} lines)", lines_read));
                        }
                        Err(_) => {
                            anyhow::bail!("Timeout reading settings from CNC (after {} lines)", lines_read);
                        }
                    }
                }

                tracing::debug!("CNC settings response: {} lines", all_settings.lines().count());
                Ok(all_settings)
            }
            CncConnectionType::Dummy => {
                anyhow::bail!("System is in fault state - CNC not connected")
            }
        }
    }

    /// Get acceleration setting for a specific axis
    pub async fn get_axis_acceleration(&self, axis: &str) -> Result<f64> {
        let settings = self.query_settings().await?;

        // Determine which setting to look for based on axis
        let setting_num = match axis.to_uppercase().as_str() {
            "X" => "120",
            "Y" => "121",
            "Z" => "122",
            "A" => "123",
            "B" => "124",
            "C" => "125",
            _ => anyhow::bail!("Invalid axis: {} (supported: X, Y, Z, A, B, C)", axis),
        };

        // Parse settings to find $12X=value
        for line in settings.lines() {
            if line.starts_with(&format!("${}", setting_num)) {
                if let Some(value_str) = line.split('=').nth(1) {
                    return value_str
                        .trim()
                        .parse::<f64>()
                        .context("Failed to parse acceleration value");
                }
            }
        }

        anyhow::bail!("Acceleration setting ${} not found in controller response", setting_num)
    }

    /// Helper to read all available lines from the CNC controller
    /// Reads lines until timeout (default 50ms), logging and discarding [MSG:...] lines
    async fn read_all_response_lines(
        reader: &mut tokio::io::BufReader<impl tokio::io::AsyncRead + Unpin>,
        timeout_ms: Option<u64>,
    ) -> Result<Vec<String>> {
        let mut lines = Vec::new();
        let mut response = String::new();
        let timeout = timeout_ms.unwrap_or(50);

        // Read first line (should always be present)
        let read_result = tokio::time::timeout(
            tokio::time::Duration::from_millis(timeout),
            reader.read_line(&mut response)
        ).await;

        match read_result {
            Ok(Ok(_)) => {
                let line = response.trim().to_string();
                if !line.is_empty() {
                    lines.push(line);
                }
            }
            Ok(Err(e)) => return Err(e).context("Failed to read from CNC"),
            Err(_) => return Err(anyhow::anyhow!("Timeout reading from CNC")),
        }

        // Continue reading additional lines with same timeout
        // This consumes any trailing messages like [MSG:...] or status responses
        loop {
            response.clear();
            let read_result = tokio::time::timeout(
                tokio::time::Duration::from_millis(timeout),
                reader.read_line(&mut response)
            ).await;

            match read_result {
                Ok(Ok(0)) => break, // EOF
                Ok(Ok(_)) => {
                    let line = response.trim().to_string();
                    if !line.is_empty() {
                        lines.push(line);
                    }
                }
                Ok(Err(_)) => break, // Read error, stop
                Err(_) => break,     // Timeout, no more data available
            }
        }

        Ok(lines)
    }

    /// Send a command to the CNC controller and wait for response
    ///
    /// # Arguments
    /// * `command` - The command to send
    /// * `expect_status_response` - If true, will read a second line if first response is "ok"
    /// * `timeout_ms` - Timeout in milliseconds for reading response (default: 1000ms)
    pub async fn send_command_with_options(
        &self,
        command: &str,
        expect_status_response: bool,
        timeout_ms: u64,
    ) -> Result<String> {
        let mut conn = self.connection.lock().await;

        let cmd = format!("{}\n", command.trim());
        tracing::debug!("Sending CNC command: {}", command);

        match &mut *conn {
            CncConnectionType::Tcp(reader) => {
                let stream = reader.get_mut();
                stream
                    .write_all(cmd.as_bytes())
                    .await
                    .context("Failed to send command to CNC")?;

                // Read all response lines (uses timeout_ms for first line, then defaults to 50ms)
                let lines = Self::read_all_response_lines(reader, Some(timeout_ms)).await?;

                // Process the lines
                self.process_response_lines(lines, expect_status_response)
            }
            CncConnectionType::Serial(reader) => {
                let stream = reader.get_mut();
                stream
                    .write_all(cmd.as_bytes())
                    .await
                    .context("Failed to send command to CNC")?;

                // Read all response lines (uses timeout_ms for first line, then defaults to 50ms)
                let lines = Self::read_all_response_lines(reader, Some(timeout_ms)).await?;

                // Process the lines
                self.process_response_lines(lines, expect_status_response)
            }
            CncConnectionType::Dummy => {
                anyhow::bail!("System is in fault state - CNC not connected")
            }
        }
    }

    /// Process response lines from CNC, filtering MSG lines and extracting the appropriate response
    fn process_response_lines(&self, lines: Vec<String>, expect_status_response: bool) -> Result<String> {
        let mut first_response = None;
        let mut status_response = None;

        for line in lines {
            tracing::debug!("CNC response line: {}", line);

            if line.starts_with("[MSG:") && line.ends_with("]") {
                // Log informational messages and discard
                let msg = &line[5..line.len()-1]; // Extract message content
                tracing::info!("CNC message: {}", msg);
            } else if line.starts_with("GrblHAL") || line.starts_with("Grbl") {
                // Boot/reset message from controller - log and discard
                tracing::debug!("CNC boot message: {}", line);
            } else if line.starts_with("<") && line.ends_with(">") {
                // Status response
                status_response = Some(line);
            } else if line == "ok" || line.starts_with("error:") {
                // Command response
                first_response = Some(line);
            } else {
                // Unexpected line
                tracing::warn!("Unexpected CNC response line: {}", line);
            }
        }

        // Return appropriate response
        if expect_status_response {
            if let Some(status) = status_response {
                Ok(status)
            } else {
                anyhow::bail!("Expected status response but none found")
            }
        } else if let Some(response) = first_response {
            if response.starts_with("error:") {
                anyhow::bail!("CNC error: {}", response)
            } else {
                Ok(response)
            }
        } else {
            anyhow::bail!("No valid response from CNC")
        }
    }

    /// Send a command to the CNC controller and wait for response (convenience wrapper)
    pub async fn send_command(&self, command: &str) -> Result<String> {
        self.send_command_with_options(command, false, 1000).await
    }

    /// Send a real-time command (single byte, no newline)
    pub async fn send_realtime_command(&self, command: u8) -> Result<()> {
        let mut conn = self.connection.lock().await;

        tracing::debug!("Sending CNC realtime command: 0x{:02X}", command);

        match &mut *conn {
            CncConnectionType::Tcp(reader) => {
                let stream = reader.get_mut();
                stream
                    .write_all(&[command])
                    .await
                    .context("Failed to send realtime command to CNC")?;
            }
            CncConnectionType::Serial(reader) => {
                let stream = reader.get_mut();
                stream
                    .write_all(&[command])
                    .await
                    .context("Failed to send realtime command to CNC")?;
            }
            CncConnectionType::Dummy => {
                anyhow::bail!("System is in fault state - CNC not connected")
            }
        }

        Ok(())
    }

    /// Home the specified axis
    pub async fn home_axis(&self, axis: &str) -> Result<String> {
        let command = format!("$H{}", axis);
        self.send_command(&command).await
    }

    /// Move to absolute position with feed rate
    pub async fn move_absolute(&self, axis: &str, position: f64, feed_rate: f64) -> Result<String> {
        let command = format!("G90 G1 {}{}F{}", axis, position, feed_rate);
        self.send_command(&command).await
    }

    /// Get current position (send ? status query)
    pub async fn get_status(&self) -> Result<String> {
        self.send_command_with_options("?", true, 1000).await
    }

    /// Send feed hold command (0x21 = '!')
    ///
    /// Pauses motion with proper deceleration according to acceleration settings ($120).
    /// This is safe for emergency stops as it respects the configured deceleration rate.
    /// The controller enters Hold state and can be resumed with cycle_start() or aborted
    /// with soft_reset().
    pub async fn feed_hold(&self) -> Result<()> {
        self.send_realtime_command(0x21).await
    }

    /// Send soft reset command (0x18 = Ctrl-X)
    ///
    /// Immediately halts all motion and clears the motion queue. Returns the controller
    /// to idle state. This is used for emergency stops where we want to abort the current
    /// operation completely (not pause it).
    ///
    /// WARNING: This does NOT respect acceleration settings and will cause immediate
    /// deceleration. For safe emergency stops, use feed_hold() first, wait for the motor
    /// to stop, then call soft_reset() to abort the program.
    pub async fn soft_reset(&self) -> Result<()> {
        self.send_realtime_command(0x18).await
    }

    /// Send queue flush command (0x19 = Ctrl-Y)
    ///
    /// Gracefully clears the command queue without triggering an alarm state.
    /// This should be used after feed_hold() to clear pending commands when stopping
    /// movement. Unlike soft_reset (0x18), this does not trigger an alarm.
    pub async fn queue_flush(&self) -> Result<()> {
        self.send_realtime_command(0x19).await
    }

    /// Parse position from status response
    /// Status format: <Idle|MPos:0.000,0.000,0.000|...>
    pub fn parse_position(status: &str, axis: &str) -> Result<f64> {
        // Look for MPos: in the status string
        let mpos_start = status
            .find("MPos:")
            .context("MPos not found in status")?;

        let coords_start = mpos_start + 5;
        let coords_end = status[coords_start..]
            .find('|')
            .map(|i| i + coords_start)
            .unwrap_or(status.len() - 1);

        let coords = &status[coords_start..coords_end];
        let parts: Vec<&str> = coords.split(',').collect();

        // Map axis to index: X=0, Y=1, Z=2, A=3, B=4, C=5
        let index = match axis.to_uppercase().as_str() {
            "X" => 0,
            "Y" => 1,
            "Z" => 2,
            "A" => 3,
            "B" => 4,
            "C" => 5,
            _ => anyhow::bail!("Invalid axis: {} (supported: X, Y, Z, A, B, C)", axis),
        };

        if index < parts.len() {
            parts[index]
                .parse::<f64>()
                .context("Failed to parse position value")
        } else {
            anyhow::bail!("Axis index {} out of bounds", index)
        }
    }

    /// Parse state from status response
    /// Status format: <Idle|...> or <Run|...> etc.
    pub fn parse_state(status: &str) -> Result<String> {
        if let Some(start) = status.find('<') {
            if let Some(end) = status.find('|') {
                return Ok(status[start + 1..end].to_string());
            }
        }
        anyhow::bail!("Failed to parse state from status")
    }

    /// Parse alarm state from status response
    /// Returns (is_alarm, alarm_code)
    /// Status format: <Alarm|...> or <Alarm:1|...> where 1 is the alarm code
    pub fn parse_alarm(status: &str) -> (bool, Option<String>) {
        if let Some(start) = status.find('<') {
            if let Some(end) = status.find('|') {
                let state = &status[start + 1..end];

                // Check if state starts with "Alarm"
                if state.starts_with("Alarm") {
                    // Check for alarm code after colon
                    if let Some(colon_pos) = state.find(':') {
                        let code = state[colon_pos + 1..].to_string();
                        return (true, Some(code));
                    } else {
                        return (true, None);
                    }
                }
            }
        }
        (false, None)
    }

    /// Validate that stop_delay_ms is sufficient for safe deceleration
    ///
    /// Calculates the minimum time required to decelerate from max speed to zero
    /// and compares it to the configured stop_delay_ms.
    ///
    /// Returns Ok(()) if valid, or an error describing the issue.
    pub async fn validate_stop_delay(
        &self,
        axis: &str,
        open_speed: f64,
        close_speed: f64,
        stop_delay_ms: u64,
    ) -> Result<()> {
        // Get acceleration from controller
        let acceleration = self.get_axis_acceleration(axis).await?;

        tracing::info!(
            "Controller {} axis acceleration: {} mm/sec²",
            axis,
            acceleration
        );

        // Find maximum speed (in mm/min, need to convert to mm/sec)
        let max_speed_mm_per_min = f64::max(open_speed, close_speed);
        let max_speed_mm_per_sec = max_speed_mm_per_min / 60.0;

        // Calculate deceleration time: time = velocity / acceleration
        let decel_time_sec = max_speed_mm_per_sec / acceleration;
        let decel_time_ms = decel_time_sec * 1000.0;

        // Add 20% safety margin
        let required_delay_ms = (decel_time_ms * 1.2).ceil() as u64;

        tracing::info!(
            "Deceleration time from {} mm/min: {:.0} ms (recommended: {} ms with 20% margin)",
            max_speed_mm_per_min,
            decel_time_ms,
            required_delay_ms
        );

        if stop_delay_ms < required_delay_ms {
            anyhow::bail!(
                "stop_delay_ms ({} ms) is too short for safe deceleration!\n\
                 Maximum speed: {:.0} mm/min ({:.1} mm/sec)\n\
                 Acceleration: {} mm/sec²\n\
                 Minimum deceleration time: {:.0} ms\n\
                 Recommended stop_delay_ms: {} ms (with 20% safety margin)\n\
                 \n\
                 Either:\n\
                 1. Increase stop_delay_ms to at least {} ms in your config, or\n\
                 2. Reduce open_speed/close_speed, or\n\
                 3. Increase controller acceleration setting ${}",
                stop_delay_ms,
                max_speed_mm_per_min,
                max_speed_mm_per_sec,
                acceleration,
                decel_time_ms,
                required_delay_ms,
                required_delay_ms,
                match axis.to_uppercase().as_str() {
                    "X" => "120",
                    "Y" => "121",
                    "Z" => "122",
                    "A" => "123",
                    "B" => "124",
                    "C" => "125",
                    _ => "12X",
                }
            );
        }

        tracing::info!(
            "✓ stop_delay_ms ({} ms) is adequate for safe deceleration",
            stop_delay_ms
        );

        Ok(())
    }
}
