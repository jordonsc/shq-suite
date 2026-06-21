"""
Argus (AI alarm assessment daemon) deployment module.

Deploys the Argus binary, the kiosk HUD `web/` assets, config, and systemd
service to atlas.

**NATIVE build only.** Argus is the ONLY Rust app in this suite that runs on
x86_64 (atlas, the HA + RAG host), so it is built with a plain
`cargo build --release` — NOT the `cross`/Podman ARM64 `build-rpi.sh` path used
by nyx/overwatch/dosa. The `--build` flag therefore runs `cargo build --release`
in `argus/` (see `deploy.py:run_cargo_release`), and the binary is taken from
`argus/target/release/argus` rather than an `argus/build/` cross-output dir.
"""

from pathlib import Path
from typing import List

from .base import BaseDeployer


class ArgusDeployer(BaseDeployer):
    """Deployer for the Argus AI alarm assessment daemon (native x86_64 → atlas)."""

    def __init__(
        self,
        hostnames: List[str],
        user: str,
        private_key: str,
        source_path: str,
        destination_path: str,
        service_name: str,
        web_path: str,
        config_file: str,
        service_file: str,
    ):
        """
        Initialize the Argus deployer.

        Args:
            hostnames: List of target hostnames to deploy to (atlas)
            user: SSH username for remote hosts (shq)
            private_key: Path to SSH private key
            source_path: Path to the native release binary (argus/target/release/argus)
            destination_path: Install directory on the remote host (relative to ~)
            service_name: Name of the systemd user service to restart (argus)
            web_path: Path to the kiosk HUD assets (argus/web) served from web.dir
            config_file: Path to config.yaml template
            service_file: Path to systemd service file
        """
        super().__init__(hostnames, user, private_key, source_path, destination_path, service_name)
        self.web_path = self._resolve_source_path(web_path)
        self.config_file = self._expand_path(config_file)
        self.service_file = self._expand_path(service_file)

    def _install_systemd_service(self, hostname: str, verbose: bool = False) -> bool:
        """Install the systemd *user* service file (mirrors overwatch/dosa)."""
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
        Deploy Argus to a single host (atlas).

        This includes:
        - Installing the native binary to ~/.local/bin/argus
        - Syncing the kiosk HUD `web/` assets (served from web.dir)
        - Copying the config file
        - Installing the systemd user service
        - Restarting the service

        Args:
            hostname: Target hostname
            verbose: Show verbose output

        Returns:
            True if successful, False otherwise
        """
        print(f"\n=== Deploying Argus to {hostname} ===")

        # The systemd unit's ExecStart is %h/.local/bin/argus; install the binary there.
        bin_dest = f"{self.destination_path}/bin/argus"

        steps = [
            ("Creating install directories", lambda: self.run_ssh_command(
                hostname, "mkdir -p ~/.local/bin ~/.config/argus ~/.config/argus/web", verbose=verbose
            )),
            ("Installing native binary", lambda: self.run_rsync(
                self.source_path, bin_dest, hostname, delete=False, verbose=verbose
            )),
            ("Marking binary executable", lambda: self.run_ssh_command(
                hostname, f"chmod +x {bin_dest}", verbose=verbose
            )),
            ("Syncing HUD web assets", lambda: self.run_rsync(
                f"{self.web_path}/", "~/.config/argus/web/", hostname, delete=True, verbose=verbose
            )),
            ("Copying config file", lambda: self.run_rsync(
                self.config_file, "~/.config/argus/config.yaml", hostname, delete=False, verbose=verbose
            )),
            ("Installing systemd service", lambda: self._install_systemd_service(hostname, verbose)),
            ("Restarting Argus service", lambda: self.run_ssh_command(
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
