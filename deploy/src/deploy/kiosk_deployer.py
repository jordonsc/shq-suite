"""
Kiosk deployment module.

This will install the display server and configure the host for kiosk mode.
"""

from pathlib import Path
from typing import List, Optional

from .base import BaseDeployer


class KioskDeployer(BaseDeployer):
    """Deployer for kiosk display applications."""

    def __init__(
        self,
        hostnames: List[str],
        user: str,
        private_key: str,
        source_path: str,
        destination_path: str,
        service_name: str,
        wallpaper_path: Optional[str] = None,
        dashboard_url: str = "http://athena.shq.sh:8123/dashboard-kiosk/{kiosk_name}?kiosk",
        kiosk_service_file: str = None,
        display_service_file: str = None,
    ):
        """
        Initialize the Kiosk deployer.

        Args:
            hostnames: List of target hostnames to deploy to
            user: SSH username for remote hosts
            private_key: Path to SSH private key
            source_path: Source directory path to deploy
            destination_path: Destination directory on remote host
            service_name: Name of the systemd service
            wallpaper_path: Optional path to wallpaper image file
            dashboard_url: Dashboard URL template with {kiosk_name} placeholder
            kiosk_service_file: Path to kiosk systemd service template
            display_service_file: Path to display systemd service template
        """
        super().__init__(hostnames, user, private_key, source_path, destination_path, service_name)

        self.wallpaper_path = Path(wallpaper_path) if wallpaper_path else None
        self.dashboard_url = dashboard_url
        self.kiosk_service_file = self._expand_path(kiosk_service_file)
        self.display_service_file = self._expand_path(display_service_file)

    def _extract_kiosk_name(self, hostname: str) -> str:
        """
        Extract the kiosk name from FQDN.

        Args:
            hostname: Full hostname (e.g., kiosk1.shq.sh)

        Returns:
            Short kiosk name (e.g., kiosk1)
        """
        return hostname.split('.')[0]

    def _copy_wallpaper(self, hostname: str, verbose: bool = False) -> bool:
        """
        Copy wallpaper file to remote host.

        Args:
            hostname: Target hostname
            verbose: Show verbose output

        Returns:
            True if successful, False otherwise
        """
        if not self.wallpaper_path or not self.wallpaper_path.exists():
            # Skip if no wallpaper configured
            print("No wallpaper configured, skipping copy.")
            return True  

        destination = f"{self.destination_path}/pi_splash.png"
        return self.run_rsync(
            self.wallpaper_path,
            destination,
            hostname,
            delete=False,
            verbose=verbose
        )

    def _create_kiosk_service(self, hostname: str, verbose: bool = False) -> bool:
        """
        Create the kiosk systemd user service file.

        Args:
            hostname: Target hostname
            verbose: Show verbose output

        Returns:
            True if successful, False otherwise
        """
        kiosk_name = self._extract_kiosk_name(hostname)
        # Substitute {kiosk_name} in the dashboard URL
        dashboard_url = self.dashboard_url.format(kiosk_name=kiosk_name)

        # Read service template and substitute dashboard URL
        with open(self.kiosk_service_file, 'r') as f:
            service_content = f.read().format(dashboard_url=dashboard_url)

        # Create the service file on remote host
        cmd = [
            f"mkdir -p ~/.config/systemd/user",
            f"cat > ~/.config/systemd/user/kiosk.service << 'EOF'\n{service_content}EOF"
        ]
        return self.run_ssh_command(hostname, cmd, verbose=verbose)

    def _create_display_service(self, hostname: str, verbose: bool = False) -> bool:
        """
        Create the display systemd user service file.

        Args:
            hostname: Target hostname
            verbose: Show verbose output

        Returns:
            True if successful, False otherwise
        """
        # Read service template
        with open(self.display_service_file, 'r') as f:
            service_content = f.read()

        # Create the service file on remote host
        cmd = [
            f"mkdir -p ~/.config/systemd/user",
            f"cat > ~/.config/systemd/user/display.service << 'EOF'\n{service_content}EOF"
        ]
        return self.run_ssh_command(hostname, cmd, verbose=verbose)

    def _enable_services(self, hostname: str, verbose: bool = False) -> bool:
        """
        Enable and start systemd user services, and enable linger.

        Args:
            hostname: Target hostname
            verbose: Show verbose output

        Returns:
            True if successful, False otherwise
        """
        cmds = [
            "systemctl --user daemon-reload",
            "gsettings set org.gnome.desktop.interface color-scheme 'prefer-dark'",
            "systemctl --user enable kiosk.service",
            "systemctl --user restart kiosk.service",
            "systemctl --user enable display.service",
            "systemctl --user restart display.service",
            f"sudo loginctl enable-linger {self.user}"
        ]

        return self.run_ssh_command(hostname, cmds, verbose=verbose)

    def _configure_desktop(self, hostname: str, verbose: bool = False) -> bool:
        """
        Configure desktop settings (wallpaper and hide icons).

        Args:
            hostname: Target hostname
            verbose: Show verbose output

        Returns:
            True if successful, False otherwise
        """
        # Use glob pattern to find desktop config files regardless of profile name or DSI port
        # This handles variations like LXDE-pi/default and DSI-1/DSI-2
        cmds = [
            "pcmanfm --set-wallpaper ~/pi_splash.png --wallpaper-mode=crop",
            "for f in ~/.config/pcmanfm/*/desktop-items-DSI-*.conf; do [ -f \"$f\" ] && sudo sed -i -r 's/^(show_[^=]+)=1$/\\1=0/' \"$f\"; done"
        ]

        return self.run_ssh_command(hostname, cmds, verbose=verbose)

    def deploy_to_host(self, hostname: str, verbose: bool = False) -> bool:
        """
        Perform initial setup of a kiosk host.

        This includes:
        - Copying display files and wallpaper
        - Creating systemd services
        - Configuring desktop

        NOTE: Manual raspi-config setup should be done first:
        - Set hostname
        - Enable auto-login (desktop)
        - Disable splash screen
        - Enable SSH

        Args:
            hostname: Target hostname
            verbose: Show verbose output

        Returns:
            True if successful, False otherwise
        """
        print(f"\n=== Setting up kiosk {hostname} ===")

        steps = [
            ("Copying display server", lambda: self.run_rsync(
                f"{self.source_path}/", f"{self.destination_path}/display/", hostname, delete=True, verbose=verbose
            )),
            ("Copying wallpaper", lambda: self._copy_wallpaper(hostname, verbose)),
            ("Creating kiosk service", lambda: self._create_kiosk_service(hostname, verbose)),
            ("Creating display service", lambda: self._create_display_service(hostname, verbose)),
            ("Enabling services", lambda: self._enable_services(hostname, verbose)),
            ("Configuring desktop", lambda: self._configure_desktop(hostname, verbose)),
        ]

        for step_name, step_func in steps:
            print(f" * {step_name}..", end="", flush=True)
            if not step_func():
                print(f" FAILED")
                return False
            print(" done")

        print(f"Setup complete for {hostname}.")
        
        return True
