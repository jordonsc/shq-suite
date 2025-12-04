"""Home Assistant deployment module."""

from typing import List

from .base import BaseDeployer


class HomeAssistantDeployer(BaseDeployer):
    """Deployer for Home Assistant custom components."""

    def __init__(
        self,
        hostnames: List[str],
        user: str,
        private_key: str,
        source_path: str,
        destination_path: str,
        service_name: str,
    ):
        """
        Initialize the Home Assistant deployer.

        Args:
            hostnames: List of target hostnames to deploy to
            user: SSH username for remote hosts
            private_key: Path to SSH private key
            source_path: Base source path for custom components
            destination_path: Destination path on remote host
            service_name: Name of the systemd service
        """
        super().__init__(hostnames, user, private_key, source_path, destination_path, service_name)

    def deploy_to_host(self, hostname: str, verbose: bool = False) -> bool:
        """
        Deploy Home Assistant component to a single host.

        Args:
            hostname: Target hostname
            verbose: Show verbose output

        Returns:
            True if successful, False otherwise
        """
        print(f"\n=== Setting up HA server at {hostname} ===")

        # Rsync the component files
        print(f" * Deploying components.. ", end="", flush=True)
        if not self.run_rsync(self.source_path, self.destination_path, hostname, verbose=verbose, delete=False):
            print("FAILED")
            return False
        
        print("done", flush=True)

        # Restart the homeassistant service (system service, not user service)
        print(f" * Restarting service.. ", end="", flush=True)
        restart_cmd = f"sudo systemctl restart {self.service_name}.service"
        if not self.run_ssh_command(hostname, restart_cmd, verbose=verbose):
            print(f"Failed to restart {self.service_name} service.")
            return False

        print("done")

        return True
