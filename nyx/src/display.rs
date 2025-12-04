use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs;
use tokio::sync::Mutex;

use crate::messages::DisplayMetrics;

/// Display controller for hardware backlight control via sysfs
#[derive(Clone)]
pub struct DisplayController {
    inner: Arc<Mutex<DisplayControllerInner>>,
}

struct DisplayControllerInner {
    backlight_path: PathBuf,
    max_brightness: u32,
    cached_brightness: u8,
}

impl DisplayController {
    /// Create a new display controller by detecting the backlight device
    pub async fn new() -> Result<Self> {
        let backlight_path = Self::detect_backlight_device().await?;

        // Read max_brightness from device
        let max_brightness_path = backlight_path.join("max_brightness");
        let max_brightness_str = fs::read_to_string(&max_brightness_path)
            .await
            .context("Failed to read max_brightness")?;
        let max_brightness: u32 = max_brightness_str
            .trim()
            .parse()
            .context("Failed to parse max_brightness")?;

        tracing::info!(
            "Display controller initialized: {:?}, max_brightness={}",
            backlight_path,
            max_brightness
        );

        // Read initial brightness
        let controller = Self {
            inner: Arc::new(Mutex::new(DisplayControllerInner {
                backlight_path,
                max_brightness,
                cached_brightness: 0,
            })),
        };

        // Update cached brightness
        let brightness = controller.get_brightness().await?;
        controller.inner.lock().await.cached_brightness = brightness;

        Ok(controller)
    }

    /// Detect the backlight device, preferring Touch Display 2
    async fn detect_backlight_device() -> Result<PathBuf> {
        let base_path = PathBuf::from("/sys/class/backlight");

        // Prefer Touch Display 2
        let touch_display_2 = base_path.join("10-0045");
        if touch_display_2.exists() {
            tracing::info!("Detected Touch Display 2 at {:?}", touch_display_2);
            return Ok(touch_display_2);
        }

        // Fall back to original display
        let original_display = base_path.join("rpi_backlight");
        if original_display.exists() {
            tracing::info!("Detected original display at {:?}", original_display);
            return Ok(original_display);
        }

        // Try to find any backlight device
        if base_path.exists() {
            let mut entries = fs::read_dir(&base_path)
                .await
                .context("Failed to read backlight directory")?;

            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                if path.is_dir() {
                    tracing::info!("Detected backlight device at {:?}", path);
                    return Ok(path);
                }
            }
        }

        Err(anyhow!("No backlight device found"))
    }

    /// Get display state (on/off)
    pub async fn get_display_state(&self) -> Result<bool> {
        let brightness = self.get_brightness().await?;
        Ok(brightness > 0)
    }

    /// Set display state (on/off)
    pub async fn set_display_state(&self, state: bool) -> Result<()> {
        let inner = self.inner.lock().await;
        let brightness = if state {
            // Use cached brightness or default to 178 (~70% brightness)
            if inner.cached_brightness > 0 {
                inner.cached_brightness
            } else {
                178
            }
        } else {
            0
        };
        drop(inner);

        tracing::info!("Setting display state to {}, brightness={}", state, brightness);
        self.set_brightness(brightness).await
    }

    /// Get brightness (0-255 scale)
    pub async fn get_brightness(&self) -> Result<u8> {
        let inner = self.inner.lock().await;
        let brightness_path = inner.backlight_path.join("brightness");

        let brightness_str = fs::read_to_string(&brightness_path)
            .await
            .context("Failed to read brightness")?;

        let raw_brightness: u32 = brightness_str
            .trim()
            .parse()
            .context("Failed to parse brightness")?;

        // Convert from device scale to 0-255 scale
        let brightness = ((raw_brightness * 255) / inner.max_brightness) as u8;
        Ok(brightness)
    }

    /// Set brightness (0-255 scale)
    pub async fn set_brightness(&self, brightness: u8) -> Result<()> {
        let mut inner = self.inner.lock().await;

        // Convert from 0-255 scale to device scale
        let raw_brightness = (brightness as u32 * inner.max_brightness) / 255;

        let brightness_path = inner.backlight_path.join("brightness");
        fs::write(&brightness_path, raw_brightness.to_string())
            .await
            .context("Failed to write brightness")?;

        // Cache brightness if > 0
        if brightness > 0 {
            inner.cached_brightness = brightness;
        }

        tracing::debug!("Set brightness to {} (raw: {})", brightness, raw_brightness);
        Ok(())
    }

    /// Get display metrics
    pub async fn get_metrics(&self) -> Result<DisplayMetrics> {
        let display_on = self.get_display_state().await?;
        let brightness = self.get_brightness().await?;

        Ok(DisplayMetrics {
            display_on,
            brightness,
        })
    }
}
