"""Configuration management for deployment."""

import os
from dataclasses import dataclass
from pathlib import Path
from typing import List, Optional

import yaml


@dataclass
class DeploymentConfig:
    """Base configuration for deployment operations."""

    hostnames: List[str]
    user: str
    private_key: str


@dataclass
class HomeAssistantConfig(DeploymentConfig):
    """Configuration for Home Assistant deployments."""

    source_path: str = None
    component_path: str = "/etc/home-assistant"
    systemd_service: str = "homeassistant"

    def __post_init__(self):
        """Set default source_path if not provided."""
        if self.source_path is None:
            # Path relative to project root: home-assistant/custom_components
            project_root = Path(__file__).parent.parent.parent.parent
            self.source_path = str(project_root / "home-assistant" / "custom_components")


@dataclass
class KioskConfig(DeploymentConfig):
    """Configuration for Kiosk deployments."""

    source_path: str = None
    install_path: str = "/home/shq/display"
    systemd_service: str = "display"
    wallpaper_local_path: Optional[str] = None
    dashboard_url: str = "http://athena.shq.sh:8123/dashboard-kiosk/{kiosk_name}?kiosk"
    kiosk_service_file: str = None
    display_service_file: str = None

    def __post_init__(self):
        """Set default paths if not provided."""
        # Compute project root: deploy/src/deploy/config.py -> ../../../../
        project_root = Path(__file__).parent.parent.parent.parent
        service_dir = Path(__file__).parent.parent.parent / "config" / "service"

        if self.source_path is None:
            # Path relative to project root: display/ or nyx/build
            self.source_path = str(project_root / "nyx" / "build")

        if self.wallpaper_local_path is None:
            # Default wallpaper path: deploy/assets/pi_splash.png
            wallpaper_path = project_root / "deploy" / "assets" / "pi_splash.png"
            if wallpaper_path.exists():
                self.wallpaper_local_path = str(wallpaper_path)

        if self.kiosk_service_file is None:
            # Default kiosk service file
            self.kiosk_service_file = str(service_dir / "kiosk" / "kiosk.service")

        if self.display_service_file is None:
            # Default display service file - choose based on language
            self.display_service_file = str(service_dir / "kiosk" / "nyx.service")


@dataclass
class OverwatchConfig(DeploymentConfig):
    """Configuration for Overwatch (voice server) deployments."""

    source_path: str = None
    sounds_path: str = None
    config_file: str = None
    service_file: str = None
    install_path: str = "overwatch"
    systemd_service: str = "overwatch"

    def __post_init__(self):
        """Set default paths if not provided."""
        project_root = Path(__file__).parent.parent.parent.parent
        config_dir = Path(__file__).parent.parent.parent / "config"
        app_config_dir = config_dir / "app"
        service_dir = config_dir / "service"

        if self.source_path is None:
            # Path relative to project root: overwatch/build
            self.source_path = str(project_root / "overwatch" / "build")

        if self.sounds_path is None:
            # Path relative to project root: overwatch/sounds
            self.sounds_path = str(project_root / "overwatch" / "sounds")

        if self.config_file is None:
            # Default config file in deploy/config/app
            self.config_file = str(app_config_dir / "overwatch.yaml")

        if self.service_file is None:
            # Default service file in deploy/config/service
            self.service_file = str(service_dir / "overwatch" / "overwatch.service")


class ConfigPresets:
    """Predefined deployment configurations loaded from YAML files."""

    _CONFIG_DIR = Path(__file__).parent.parent.parent / "config"
    _DEPLOYMENT_DIR = _CONFIG_DIR / "deployment"
    _APP_CONFIG_DIR = _CONFIG_DIR / "app"
    _SERVICE_DIR = _CONFIG_DIR / "service"

    @classmethod
    def _load_yaml(cls, filename: str) -> dict:
        """Load and parse a YAML configuration file."""
        config_path = cls._DEPLOYMENT_DIR / filename
        if not config_path.exists():
            raise FileNotFoundError(f"Configuration file not found: {config_path}")

        with open(config_path, "r") as f:
            return yaml.safe_load(f) or {}

    @classmethod
    def get_ha_config(cls) -> HomeAssistantConfig:
        """Get Home Assistant deployment configuration from deployment/ha.yaml."""
        config = cls._load_yaml("ha.yaml")
        auth = config.get("auth", {})
        hass = config.get("hass", {})

        return HomeAssistantConfig(
            hostnames=config.get("hosts", []),
            user=auth.get("username", ""),
            private_key=auth.get("private_key", ""),
            source_path=hass.get("source_path"),  # None if not specified, will use default
            component_path=hass.get("component_path", "/etc/home-assistant/custom_components"),
            systemd_service=hass.get("systemd_service", "homeassistant"),
        )

    @classmethod
    def get_kiosk_config(cls) -> KioskConfig:
        """Get Kiosk deployment configuration from deployment/kiosk.yaml."""
        config = cls._load_yaml("kiosk.yaml")
        auth = config.get("auth", {})
        display = config.get("display", {})
        wallpaper = config.get("wallpaper", {})
        kiosk = config.get("kiosk", {})

        return KioskConfig(
            hostnames=config.get("hosts", []),
            user=auth.get("username", ""),
            private_key=auth.get("private_key", ""),
            source_path=display.get("source_path"),  # None if not specified, will use default
            install_path=display.get("install_path", "/home/shq"),
            systemd_service=display.get("systemd_service", "display"),
            wallpaper_local_path=wallpaper.get("local_path"),
            dashboard_url=kiosk.get("dashboard_url", ""),
            kiosk_service_file=kiosk.get("kiosk_service_file"),  # None if not specified, will use default
            display_service_file=display.get("display_service_file"),  # None if not specified, will use default
        )

    @classmethod
    def get_overwatch_config(cls) -> OverwatchConfig:
        """Get Overwatch deployment configuration from deployment/overwatch.yaml."""
        config = cls._load_yaml("overwatch.yaml")
        auth = config.get("auth", {})
        overwatch = config.get("overwatch", {})

        return OverwatchConfig(
            hostnames=config.get("hosts", []),
            user=auth.get("username", ""),
            private_key=auth.get("private_key", ""),
            source_path=overwatch.get("source_path"),  # None if not specified, will use default
            sounds_path=overwatch.get("sounds_path"),  # None if not specified, will use default
            config_file=overwatch.get("config_file"),  # None if not specified, will use default
            service_file=overwatch.get("service_file"),  # None if not specified, will use default
            install_path=overwatch.get("install_path", "overwatch"),
            systemd_service=overwatch.get("systemd_service", "overwatch"),
        )
