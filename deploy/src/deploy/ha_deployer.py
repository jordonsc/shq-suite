"""Home Assistant deployment module."""

import os
import urllib.error
import urllib.request
from pathlib import Path
from typing import List, Optional, Sequence, Tuple

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
        components: Optional[Sequence[str]] = None,
    ):
        """
        Initialize the Home Assistant deployer.

        Args:
            hostnames: List of target hostnames to deploy to
            user: SSH username for remote hosts
            private_key: Path to SSH private key
            source_path: Base source path for custom components
            destination_path: Destination path on remote host (the HA config
                root, e.g. /etc/hass — custom_components/ hangs off it)
            service_name: Name of the systemd service
            components: The custom components to deploy. `None` means the WHOLE
                `custom_components/` tree, which is the behaviour that caused
                `shq-suite-0033` — the CLI only ever passes `None` for an
                explicit `--all-components`.
        """
        super().__init__(hostnames, user, private_key, source_path, destination_path, service_name)
        # configuration.yaml lives in deploy/config/ha/ (gitignored, user-specific)
        self.config_file = Path(__file__).parent.parent.parent / "config" / "ha" / "configuration.yaml"
        # www/ directory for custom frontend resources (icons, etc.)
        self.www_path = Path(source_path).parent / "www"
        # blueprints/ directory for automation blueprints
        self.blueprints_path = Path(source_path).parent / "blueprints"
        # Which custom components to push. None == the whole tree (opt-in only).
        self.components: Optional[List[str]] = list(components) if components else None

    @property
    def custom_components_dest(self) -> str:
        """Remote `custom_components/` directory, beneath the HA config root."""
        return str(Path(self.destination_path) / "custom_components")

    def resolve_component_sources(self) -> List[Tuple[str, str]]:
        """
        Work out the (source, destination) rsync pairs for the components.

        Scoped deploys sync `<source>/<component>` into `<dest>/custom_components`,
        so a deploy touches ONLY the named component. An unscoped deploy keeps the
        legacy whole-tree behaviour.

        Raises:
            FileNotFoundError: if a named component does not exist locally. This
                is deliberately checked BEFORE any remote write — a typo must
                fail locally rather than push a partial tree.
        """
        if self.components is None:
            # Legacy whole-tree sync: `<source>` -> `<dest>`, landing the
            # `custom_components` directory itself inside the config root.
            return [(str(self.source_path), str(self.destination_path))]

        pairs: List[Tuple[str, str]] = []
        for component in self.components:
            source = Path(self.source_path) / component
            if not source.is_dir():
                raise FileNotFoundError(
                    f"custom component '{component}' not found at {source}"
                )
            pairs.append((str(source), self.custom_components_dest))
        return pairs

    def _reload_ha_config(self, verbose: bool = False) -> bool:
        """
        Reload Home Assistant configuration via the REST API.

        Uses $HA_URL and $HA_TOKEN environment variables.

        Returns:
            True if successful, False otherwise
        """
        ha_url = os.environ.get("HA_URL")
        ha_token = os.environ.get("HA_TOKEN")

        if not ha_url or not ha_token:
            print("FAILED")
            print("  $HA_URL and $HA_TOKEN must be set for config reload.")
            print("  Use --restart for a full service restart instead.")
            return False

        url = f"{ha_url}/api/services/homeassistant/reload_all"
        headers = {
            "Authorization": f"Bearer {ha_token}",
            "Content-Type": "application/json",
        }

        try:
            req = urllib.request.Request(url, data=b"{}", headers=headers, method="POST")
            with urllib.request.urlopen(req, timeout=30) as resp:
                if verbose:
                    print(f"({resp.status}) ", end="", flush=True)
            return True
        except urllib.error.HTTPError as e:
            print(f"FAILED ({e.code})")
            if verbose and e.read:
                print(f"  {e.read().decode()}")
            return False
        except urllib.error.URLError as e:
            print(f"FAILED ({e.reason})")
            return False

    def deploy_to_host(self, hostname: str, verbose: bool = False, restart: bool = False) -> bool:
        """
        Deploy Home Assistant component to a single host.

        Args:
            hostname: Target hostname
            verbose: Show verbose output
            restart: Full service restart instead of config reload

        Returns:
            True if successful, False otherwise
        """
        print(f"\n=== Setting up HA server at {hostname} ===")

        # Rsync the component files. A scoped deploy pushes ONLY the named
        # components; see `shq-suite-0033` for why the whole-tree sync is not
        # the default any more.
        try:
            pairs = self.resolve_component_sources()
        except FileNotFoundError as e:
            print(f" * Deploying components.. FAILED\n   {e}")
            return False

        scope = "the whole custom_components tree" if self.components is None else ", ".join(self.components)
        print(f" * Deploying components ({scope}).. ", end="", flush=True)
        for source, destination in pairs:
            if not self.run_rsync(source, destination, hostname, verbose=verbose, delete=False):
                print("FAILED")
                return False
        print("done", flush=True)

        # Deploy www/ directory if it exists (needs sudo as /etc/hass is root-owned)
        if self.www_path.exists():
            print(f" * Deploying www/.. ", end="", flush=True)
            tmp_www = "/tmp/ha_www"
            # destination_path is e.g. /etc/hass, www goes inside it
            final_www = str(Path(self.destination_path) / "www")
            if not self.run_rsync(str(self.www_path) + "/", tmp_www, hostname, verbose=verbose, delete=False):
                print("FAILED")
                return False
            if not self.run_ssh_command(hostname, f"sudo cp -a {tmp_www}/. {final_www}/ && rm -rf {tmp_www}", verbose=verbose):
                print("FAILED")
                return False
            print("done", flush=True)

        # Deploy blueprints/ directory if it exists (needs sudo as /etc/hass is root-owned)
        if self.blueprints_path.exists():
            print(f" * Deploying blueprints/.. ", end="", flush=True)
            tmp_bp = "/tmp/ha_blueprints"
            final_bp = str(Path(self.destination_path) / "blueprints")
            if not self.run_rsync(str(self.blueprints_path) + "/", tmp_bp, hostname, verbose=verbose, delete=False):
                print("FAILED")
                return False
            if not self.run_ssh_command(hostname, f"sudo mkdir -p {final_bp} && sudo cp -a {tmp_bp}/. {final_bp}/ && rm -rf {tmp_bp}", verbose=verbose):
                print("FAILED")
                return False
            print("done", flush=True)

        # Deploy configuration.yaml if it exists (needs sudo as /etc/hass is root-owned)
        if self.config_file.exists():
            print(f" * Deploying configuration.yaml.. ", end="", flush=True)
            tmp_dest = "/tmp/ha_configuration.yaml"
            final_dest = self.destination_path + "/configuration.yaml"
            if not self.run_rsync(self.config_file, tmp_dest, hostname, verbose=verbose, delete=False):
                print("FAILED")
                return False
            if not self.run_ssh_command(hostname, f"sudo mv {tmp_dest} {final_dest}", verbose=verbose):
                print("FAILED")
                return False
            print("done", flush=True)

        if restart:
            # Full service restart via systemd
            print(f" * Restarting service.. ", end="", flush=True)
            restart_cmd = f"sudo systemctl restart {self.service_name}.service"
            if not self.run_ssh_command(hostname, restart_cmd, verbose=verbose):
                print(f"Failed to restart {self.service_name} service.")
                return False
            print("done")
        else:
            # Reload configuration via HA REST API
            print(f" * Reloading configuration.. ", end="", flush=True)
            if not self._reload_ha_config(verbose=verbose):
                return False
            print("done")

        return True
