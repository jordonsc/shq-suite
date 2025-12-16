"""
DOSA (Door Opening Sensor Automation) deployment module.

Deploys the DOSA door automation binary, config, and systemd service.
"""

from pathlib import Path
from typing import List

from .base import BaseDeployer


class DosaDeployer(BaseDeployer):
    """Deployer for DOSA door automation server."""

    def __init__(
        self,
        hostnames: List[str],
        user: str,
        private_key: str,
        source_path: str,
        destination_path: str,
        service_name: str,
        config_file: str,
        service_file: str,
    ):
        """
        Initialize the DOSA deployer.

        Args:
            hostnames: List of target hostnames to deploy to
            user: SSH username for remote hosts
            private_key: Path to SSH private key
            source_path: Source directory path (typically dosa/build)
            destination_path: Destination directory on remote host
            service_name: Name of the systemd service to restart
            config_file: Path to config.yaml template
            service_file: Path to systemd service file
        """
        super().__init__(hostnames, user, private_key, source_path, destination_path, service_name)
        self.config_file = self._expand_path(config_file)
        self.service_file = self._expand_path(service_file)

    def _install_systemd_service(self, hostname: str, verbose: bool = False) -> bool:
        """
        Install systemd user service file.

        Args:
            hostname: Target hostname
            verbose: Show verbose output

        Returns:
            True if successful, False otherwise
        """
        with open(self.service_file, 'r') as f:
            service_content = f.read()

        commands = [
            f"mkdir -p ~/.config/systemd/user",
            f"cat > ~/.config/systemd/user/{self.service_name}.service << 'EOF'\n{service_content}EOF",
            "systemctl --user daemon-reload",
            f"systemctl --user enable {self.service_name}.service",
            f"sudo loginctl enable-linger {self.user}",
        ]

        return self.run_ssh_command(hostname, commands, verbose=verbose)

    def deploy_to_host(self, hostname: str, verbose: bool = False) -> bool:
        """
        Deploy DOSA to a single host.

        This includes:
        - Syncing binary files
        - Copying config file
        - Installing systemd service
        - Restarting service

        Args:
            hostname: Target hostname
            verbose: Show verbose output

        Returns:
            True if successful, False otherwise
        """
        print(f"\n=== Deploying DOSA to {hostname} ===")

        steps = [
            ("Syncing DOSA application", lambda: self.run_rsync(
                f"{self.source_path}/", f"{self.destination_path}/", hostname, delete=True, verbose=verbose
            )),
            ("Copying config file", lambda: self.run_rsync(
                self.config_file, f"{self.destination_path}/config.yaml", hostname, delete=False, verbose=verbose
            )),
            ("Installing systemd service", lambda: self._install_systemd_service(hostname, verbose)),
            ("Restarting DOSA service", lambda: self.run_ssh_command(
                hostname, f"systemctl --user restart {self.service_name}", verbose=verbose
            )),
        ]

        for step_name, step_func in steps:
            print(f" * {step_name}..", end="", flush=True)
            if not step_func():
                print(" FAILED")
                return False
            print(" done")

        print(f"Deployment complete for {hostname}.")
        return True
