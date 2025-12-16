#!/usr/bin/env python3
"""
SHQ Display Deployment CLI

Modular, object-oriented deployment tool for SHQ Display components.
"""

import subprocess
import sys
from pathlib import Path

import click

from deploy import DosaDeployer, HomeAssistantDeployer, KioskDeployer, OverwatchDeployer
from deploy.config import ConfigPresets


def get_project_root() -> Path:
    """Get the project root directory (shq-suite/)."""
    # This file is at deploy/src/deploy.py, so project root is 3 levels up
    return Path(__file__).resolve().parent.parent.parent


def run_build_script(project_name: str, verbose: bool = False) -> bool:
    """
    Run build-rpi.sh script for a project.

    Args:
        project_name: Name of the project (e.g., 'nyx', 'overwatch')
        verbose: Show build output

    Returns:
        True if build succeeded, False otherwise
    """
    project_root = get_project_root()
    project_dir = project_root / project_name
    build_script = project_dir / "build-rpi.sh"

    if not build_script.exists():
        click.echo(f"Error: Build script not found at {build_script}", err=True)
        return False

    click.echo(f"Building {project_name}...")

    try:
        result = subprocess.run(
            ["bash", str(build_script)],
            cwd=str(project_dir),
            capture_output=not verbose,
            text=True,
            check=True
        )

        if verbose and result.stdout:
            click.echo(result.stdout)

        click.echo(f"✓ {project_name} build complete")
        return True

    except subprocess.CalledProcessError as e:
        click.echo(f"✗ {project_name} build failed", err=True)
        if e.stdout:
            click.echo(e.stdout, err=True)
        if e.stderr:
            click.echo(e.stderr, err=True)
        return False


@click.group()
@click.version_option(version="1.0.0")
def cli():
    """
    SHQ Display Deployment Tool.

    Deploy Home Assistant custom components and Kiosk displays to remote hosts.
    """
    pass

### HOME ASSISTANT DEPLOYMENT ###
@cli.command()
@click.option(
    "--hostname",
    "-h",
    multiple=True,
    help="Override default hostname(s). Can be specified multiple times.",
)
@click.option(
    "--user",
    "-u",
    help="Override default SSH user.",
)
@click.option(
    "--key",
    "-k",
    help="Override default SSH private key path.",
)
@click.option(
    "--verbose",
    "-v",
    is_flag=True,
    help="Show verbose output.",
)
@click.option(
    "--component",
    "-c",
    default="shq_display",
    help="Name of the custom component to deploy (default: shq_display).",
)
def ha(hostname, user, key, verbose, component):
    """
    Deploy Home Assistant custom component.

    Deploys the SHQ Display custom component to Home Assistant hosts
    and restarts the homeassistant service.

    Configuration loaded from config/deployment/ha.yaml
    """
    config = ConfigPresets.get_ha_config()

    # Override defaults if provided
    hostnames = list(hostname) if hostname else config.hostnames
    ssh_user = user if user else config.user
    ssh_key = key if key else config.private_key

    click.echo(f"Deploying Home Assistant component '{component}' to {len(hostnames)} host(s)...")

    deployer = HomeAssistantDeployer(
        hostnames=hostnames,
        user=ssh_user,
        private_key=ssh_key,
        source_path=config.source_path,
        destination_path=config.component_path,
        service_name=config.systemd_service,
    )

    deployer.deploy_all(verbose=verbose)

    click.echo()
    click.echo("Home Assistant deployment complete.")


### KIOSK DEPLOYMENT ###
@cli.command()
@click.option(
    "--hostname",
    "-h",
    multiple=True,
    help="Override default hostname(s). Can be specified multiple times.",
)
@click.option(
    "--user",
    "-u",
    help="Override default SSH user.",
)
@click.option(
    "--key",
    "-k",
    help="Override default SSH private key path.",
)
@click.option(
    "--verbose",
    "-v",
    is_flag=True,
    help="Show verbose output.",
)
@click.option(
    "--destination",
    "-d",
    help="Override destination directory on remote host.",
)
@click.option(
    "--build",
    "-b",
    is_flag=True,
    help="Build nyx binary before deploying.",
)
def kiosk(hostname, user, key, verbose, destination, build):
    """
    Deploy Nyx display application & kiosk service.

    Deploys the entire project to kiosk hosts and restarts the display service.

    Configuration loaded from config/deployment/kiosk.yaml
    """
    # Build nyx if --build flag is set
    if build:
        if not run_build_script("nyx", verbose=verbose):
            click.echo("Build failed. Aborting deployment.", err=True)
            sys.exit(1)
        click.echo()

    config = ConfigPresets.get_kiosk_config()

    # Override defaults if provided
    hostnames = list(hostname) if hostname else config.hostnames
    ssh_user = user if user else config.user
    ssh_key = key if key else config.private_key
    destination_dir = destination if destination else config.install_path

    click.echo(f"Deploying Kiosk application to {len(hostnames)} host(s)...")

    deployer = KioskDeployer(
        hostnames=hostnames,
        user=ssh_user,
        private_key=ssh_key,
        source_path=config.source_path,
        destination_path=destination_dir,
        service_name=config.systemd_service,
        wallpaper_path=config.wallpaper_local_path,
        dashboard_url=config.dashboard_url,
        kiosk_service_file=config.kiosk_service_file,
        display_service_file=config.display_service_file,
    )

    deployer.deploy_all(verbose=verbose)

    click.echo()
    click.echo("Kiosk deployment complete.")


### OVERWATCH DEPLOYMENT ###
@cli.command()
@click.option(
    "--hostname",
    "-h",
    multiple=True,
    help="Override default hostname(s). Can be specified multiple times.",
)
@click.option(
    "--user",
    "-u",
    help="Override default SSH user.",
)
@click.option(
    "--key",
    "-k",
    help="Override default SSH private key path.",
)
@click.option(
    "--verbose",
    "-v",
    is_flag=True,
    help="Show verbose output.",
)
@click.option(
    "--destination",
    "-d",
    help="Override destination directory on remote host.",
)
@click.option(
    "--build",
    "-b",
    is_flag=True,
    help="Build overwatch binary before deploying.",
)
def overwatch(hostname, user, key, verbose, destination, build):
    """
    Deploy Overwatch voice server.

    Deploys the voice server binary to remote hosts and restarts the voice service.

    Configuration loaded from config/deployment/overwatch.yaml
    """
    # Build overwatch if --build flag is set
    if build:
        if not run_build_script("overwatch", verbose=verbose):
            click.echo("Build failed. Aborting deployment.", err=True)
            sys.exit(1)
        click.echo()

    config = ConfigPresets.get_overwatch_config()

    # Override defaults if provided
    hostnames = list(hostname) if hostname else config.hostnames
    ssh_user = user if user else config.user
    ssh_key = key if key else config.private_key
    destination_dir = destination if destination else config.install_path

    click.echo(f"Deploying Overwatch voice server to {len(hostnames)} host(s)...")

    deployer = OverwatchDeployer(
        hostnames=hostnames,
        user=ssh_user,
        private_key=ssh_key,
        source_path=config.source_path,
        destination_path=destination_dir,
        service_name=config.systemd_service,
        sounds_path=config.sounds_path,
        config_file=config.config_file,
        service_file=config.service_file,
    )

    deployer.deploy_all(verbose=verbose)

    click.echo()
    click.echo("Overwatch deployment complete.")


### DOSA DEPLOYMENT ###
@cli.command()
@click.option(
    "--hostname",
    "-h",
    multiple=True,
    help="Override default hostname(s). Can be specified multiple times.",
)
@click.option(
    "--user",
    "-u",
    help="Override default SSH user.",
)
@click.option(
    "--key",
    "-k",
    help="Override default SSH private key path.",
)
@click.option(
    "--verbose",
    "-v",
    is_flag=True,
    help="Show verbose output.",
)
@click.option(
    "--destination",
    "-d",
    help="Override destination directory on remote host.",
)
@click.option(
    "--build",
    "-b",
    is_flag=True,
    help="Build dosa binary before deploying.",
)
def dosa(hostname, user, key, verbose, destination, build):
    """
    Deploy DOSA door automation server.

    Deploys the door automation binary to remote hosts and restarts the DOSA service.

    Configuration loaded from config/deployment/dosa.yaml
    """
    # Build dosa if --build flag is set
    if build:
        if not run_build_script("dosa", verbose=verbose):
            click.echo("Build failed. Aborting deployment.", err=True)
            sys.exit(1)
        click.echo()

    config = ConfigPresets.get_dosa_config()

    # Override defaults if provided
    hostnames = list(hostname) if hostname else config.hostnames
    ssh_user = user if user else config.user
    ssh_key = key if key else config.private_key
    destination_dir = destination if destination else config.install_path

    click.echo(f"Deploying DOSA door automation to {len(hostnames)} host(s)...")

    deployer = DosaDeployer(
        hostnames=hostnames,
        user=ssh_user,
        private_key=ssh_key,
        source_path=config.source_path,
        destination_path=destination_dir,
        service_name=config.systemd_service,
        config_file=config.config_file,
        service_file=config.service_file,
    )

    deployer.deploy_all(verbose=verbose)

    click.echo()
    click.echo("DOSA deployment complete.")


if __name__ == "__main__":
    cli()
