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

    /// Query all grblHAL settings ($$)
    /// Returns a map of setting names to values (e.g., "$120" -> "1000.000")
    /// Settings are sorted numerically by the number after the $ sign
    pub async fn query_settings(&self) -> Result<indexmap::IndexMap<String, String>> {
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
                // Use Vec to collect, then sort numerically
                let mut settings_vec = Vec::new();
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

                            // Parse setting line: $120=1000.000
                            if let Some(eq_pos) = trimmed.find('=') {
                                let setting_name = trimmed[..eq_pos].to_string();
                                let setting_value = trimmed[eq_pos + 1..].to_string();
                                settings_vec.push((setting_name, setting_value));
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

                // Sort numerically by extracting the number from "$XXX"
                settings_vec.sort_by(|a, b| {
                    let num_a = a.0.trim_start_matches('$').parse::<u32>().unwrap_or(0);
                    let num_b = b.0.trim_start_matches('$').parse::<u32>().unwrap_or(0);
                    num_a.cmp(&num_b)
                });

                // Convert to IndexMap to preserve insertion order
                let settings: indexmap::IndexMap<String, String> = settings_vec.into_iter().collect();

                tracing::debug!("CNC settings response: {} settings", settings.len());
                Ok(settings)
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
                // Use Vec to collect, then sort numerically
                let mut settings_vec = Vec::new();
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

                            // Parse setting line: $120=1000.000
                            if let Some(eq_pos) = trimmed.find('=') {
                                let setting_name = trimmed[..eq_pos].to_string();
                                let setting_value = trimmed[eq_pos + 1..].to_string();
                                settings_vec.push((setting_name, setting_value));
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

                // Sort numerically by extracting the number from "$XXX"
                settings_vec.sort_by(|a, b| {
                    let num_a = a.0.trim_start_matches('$').parse::<u32>().unwrap_or(0);
                    let num_b = b.0.trim_start_matches('$').parse::<u32>().unwrap_or(0);
                    num_a.cmp(&num_b)
                });

                // Convert to IndexMap to preserve insertion order
                let settings: indexmap::IndexMap<String, String> = settings_vec.into_iter().collect();

                tracing::debug!("CNC settings response: {} settings", settings.len());
                Ok(settings)
            }
            CncConnectionType::Dummy => {
                anyhow::bail!("System is in fault state - CNC not connected")
            }
        }
    }

    /// Get a specific CNC setting by name (e.g., "$120")
    /// Returns the value as a string
    pub async fn get_setting(&self, setting_name: &str) -> Result<String> {
        let settings = self.query_settings().await?;

        settings.get(setting_name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Setting {} not found", setting_name))
    }

    /// Set a specific CNC setting
    /// Sends the setting command to the controller (e.g., "$120=1000")
    pub async fn set_setting(&self, setting_name: &str, value: &str) -> Result<()> {
        // Validate setting name format (should start with $)
        if !setting_name.starts_with('$') {
            anyhow::bail!("Invalid setting name: {} (must start with $)", setting_name);
        }

        let cmd = format!("{}={}", setting_name, value);
        self.send_command(&cmd).await?;

        tracing::info!("Set CNC setting: {} = {}", setting_name, value);
        Ok(())
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
            } else if line.starts_with("ALARM:") {
                // Alarm notification from controller (can occur asynchronously)
                let alarm_code = line.strip_prefix("ALARM:").unwrap_or("unknown");
                tracing::error!("CNC ALARM triggered: Code {}", alarm_code);
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
    ///
    /// Homing is special: grblHAL enters Home mode immediately, then completes the
    /// two-stage homing cycle (fast seek + slow approach), which can take 30+ seconds.
    /// We handle the entire sequence here instead of returning immediately.
    pub async fn home_axis(&self, axis: &str) -> Result<String> {
        let command = format!("$H{}", axis);

        tracing::debug!("Sending CNC homing command: {}", &command);

        let mut conn = self.connection.lock().await;
        let cmd = format!("{}\n", command.trim());

        // Send the homing command
        match &mut *conn {
            CncConnectionType::Tcp(reader) => {
                // Send homing command
                {
                    let stream = reader.get_mut();
                    stream.write_all(cmd.as_bytes()).await
                        .context("Failed to send homing command to CNC")?;
                    stream.flush().await
                        .context("Failed to flush homing command to CNC")?;
                }

                // Read immediate status response
                let mut line = String::new();
                tokio::time::timeout(
                    tokio::time::Duration::from_secs(2),
                    reader.read_line(&mut line)
                ).await
                    .context("Timeout waiting for homing to start")??;

                tracing::debug!("Homing started: {}", line.trim());

                // Wait for grblHAL to send status update when homing completes
                // Keep reading lines until we see Idle state or timeout
                let start_time = tokio::time::Instant::now();
                let timeout_duration = tokio::time::Duration::from_secs(60);

                loop {
                    let remaining_time = timeout_duration.saturating_sub(start_time.elapsed());
                    if remaining_time.is_zero() {
                        return Err(anyhow::anyhow!("Homing timeout after 60 seconds"));
                    }

                    line.clear();
                    match tokio::time::timeout(remaining_time, reader.read_line(&mut line)).await {
                        Ok(Ok(_)) => {
                            let response = line.trim();
                            tracing::debug!("Homing response: {}", response);

                            // Check for completion
                            if response == "ok" {
                                tracing::info!("Homing completed after {:.1}s", start_time.elapsed().as_secs_f32());
                                return Ok("ok".to_string());
                            }

                            // Check for alarm in status responses
                            if let Ok(state) = Self::parse_state(response) {
                                if state.starts_with("Alarm") {
                                    return Err(anyhow::anyhow!("Homing failed: {}", state));
                                }
                            }

                            // Ignore blank MSG lines and status updates, keep waiting
                        }
                        Ok(Err(e)) => {
                            return Err(anyhow::anyhow!("Error reading during homing: {}", e));
                        }
                        Err(_) => {
                            return Err(anyhow::anyhow!("Homing timeout - no response from controller"));
                        }
                    }
                }
            }
            CncConnectionType::Serial(reader) => {
                // Send homing command
                {
                    let stream = reader.get_mut();
                    stream.write_all(cmd.as_bytes()).await
                        .context("Failed to send homing command to CNC")?;
                    stream.flush().await
                        .context("Failed to flush homing command to CNC")?;
                }

                // Read immediate status response
                let mut line = String::new();
                tokio::time::timeout(
                    tokio::time::Duration::from_secs(2),
                    reader.read_line(&mut line)
                ).await
                    .context("Timeout waiting for homing to start")??;

                tracing::debug!("Homing started: {}", line.trim());

                // Wait for grblHAL to send status update when homing completes
                // Keep reading lines until we see Idle state or timeout
                let start_time = tokio::time::Instant::now();
                let timeout_duration = tokio::time::Duration::from_secs(60);

                loop {
                    let remaining_time = timeout_duration.saturating_sub(start_time.elapsed());
                    if remaining_time.is_zero() {
                        return Err(anyhow::anyhow!("Homing timeout after 60 seconds"));
                    }

                    line.clear();
                    match tokio::time::timeout(remaining_time, reader.read_line(&mut line)).await {
                        Ok(Ok(_)) => {
                            let response = line.trim();
                            tracing::debug!("Homing response: {}", response);

                            // Check for completion
                            if response == "ok" {
                                tracing::info!("Homing completed after {:.1}s", start_time.elapsed().as_secs_f32());
                                return Ok("ok".to_string());
                            }

                            // Check for alarm in status responses
                            if let Ok(state) = Self::parse_state(response) {
                                if state.starts_with("Alarm") {
                                    return Err(anyhow::anyhow!("Homing failed: {}", state));
                                }
                            }

                            // Ignore blank MSG lines and status updates, keep waiting
                        }
                        Ok(Err(e)) => {
                            return Err(anyhow::anyhow!("Error reading during homing: {}", e));
                        }
                        Err(_) => {
                            return Err(anyhow::anyhow!("Homing timeout - no response from controller"));
                        }
                    }
                }
            }
            CncConnectionType::Dummy => {
                Err(anyhow::anyhow!("System is in fault state - CNC not connected"))
            }
        }
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

    /// Check if an error is a connection/communication error (should trigger reconnect)
    /// vs a grblHAL command error (should not trigger reconnect)
    ///
    /// Connection errors: I/O errors, connection closed
    /// Command errors: grblHAL error codes, operation timeouts, homing failures
    pub fn is_connection_error(err: &anyhow::Error) -> bool {
        let err_msg = err.to_string().to_lowercase();

        // These should NOT trigger reconnection
        if err_msg.contains("cnc error: error:") ||
           err_msg.contains("homing timeout") ||
           err_msg.contains("homing failed") {
            return false;
        }

        // These indicate connection/communication problems and should trigger reconnection
        err_msg.contains("failed to send") ||
        err_msg.contains("failed to read") ||
        err_msg.contains("connection closed") ||
        err_msg.contains("failed to flush") ||
        err_msg.contains("failed to connect")
    }

    /// Close the connection explicitly
    /// This is important for serial connections to release the port before reconnecting
    pub async fn close(&self) {
        let mut conn = self.connection.lock().await;
        *conn = CncConnectionType::Dummy;
        drop(conn);

        // Give the OS time to release the resource (especially important for serial)
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        tracing::debug!("CNC connection closed");
    }
}
