"""Base deployment class with shared functionality."""

import os
import subprocess
import sys
from abc import ABC, abstractmethod
from pathlib import Path
from typing import List, Union


class BaseDeployer(ABC):
    """Abstract base class for deployment operations."""

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
        Initialize the deployer.

        Args:
            hostnames: List of target hostnames to deploy to
            user: SSH username for remote hosts
            private_key: Path to SSH private key (supports ~ expansion)
            source_path: Source directory/file path to deploy (relative to project root)
            destination_path: Destination path on remote host
            service_name: Name of the systemd service to restart
        """
        self.hostnames = hostnames
        self.user = user
        self.private_key = self._expand_path(private_key)
        self.source_path = self._resolve_source_path(source_path)
        self.destination_path = destination_path
        self.service_name = service_name

    @staticmethod
    def _expand_path(path: str) -> Path:
        """Expand ~ and environment variables in path."""
        return Path(os.path.expanduser(os.path.expandvars(path)))

    @staticmethod
    def _get_project_root() -> Path:
        """Get the project root directory."""
        # Navigate up from src/deploy to project root
        return Path(__file__).parent.parent.parent.resolve()

    @classmethod
    def _resolve_source_path(cls, source_path: str) -> Path:
        """Resolve source path relative to project root."""
        project_root = cls._get_project_root()
        resolved = project_root / source_path
        return resolved.resolve()

    def run_rsync(
        self,
        source: Path,
        destination: str,
        hostname: str,
        delete: bool = True,
        verbose: bool = False,
    ) -> bool:
        """
        Run rsync command to deploy files.

        Args:
            source: Local source path
            destination: Remote destination path
            hostname: Target hostname
            delete: Whether to delete files not in source
            verbose: Show verbose output

        Returns:
            True if successful, False otherwise
        """
        ssh_cmd = f'ssh -i "{self.private_key}" -o IdentitiesOnly=yes'
        rsync_args = [
            "rsync",
            "-av",
            "--progress" if verbose else "--quiet",
        ]

        if delete:
            rsync_args.append("--delete")

        # Exclude common artifacts that may cause permission issues
        exclude_patterns = [
            "__pycache__",
            "*.pyc",
            "*.pyo",
            ".git",
            ".gitignore",
            ".DS_Store",
            "cache/",
        ]
        for pattern in exclude_patterns:
            rsync_args.extend(["--exclude", pattern])

        rsync_args.extend([
            "-e", ssh_cmd,
            str(source),
            f"{self.user}@{hostname}:{destination}",
        ])

        try:
            if verbose:
                print(f"Running rsync command: {' '.join(rsync_args)}")
            result = subprocess.run(
                rsync_args,
                check=True,
                capture_output=not verbose,
                text=True,
            )
            return result.returncode == 0
        except subprocess.CalledProcessError as e:
            # Exit code 23 means partial transfer due to some files being skipped
            # This is usually okay if it's just permission issues with __pycache__
            if e.returncode == 23:
                if verbose or (e.stderr and "__pycache__" in e.stderr):
                    print(f"\nrsync warning: Some files couldn't be deleted (code 23)")
                    if e.stderr:
                        # Only show __pycache__ related errors as warnings
                        for line in e.stderr.split('\n'):
                            if '__pycache__' in line or 'Permission denied' in line:
                                if verbose:
                                    print(f"  {line}")
                # Treat as success since main files were transferred
                return True

            # Other errors are real failures
            print(f"\nrsync failed with exit code {e.returncode}")
            if e.stdout:
                print(f"stdout: {e.stdout}")
            if e.stderr:
                print(f"stderr: {e.stderr}")
            return False

    def run_ssh_command(
        self,
        hostname: str,
        command: Union[str, List[str]],
        verbose: bool = False,
    ) -> bool:
        """
        Execute a command on the remote host via SSH.

        Args:
            hostname: Target hostname
            command: Command to execute
            verbose: Show verbose output

        Returns:
            True if successful, False otherwise
        """
        # Check if command is a list and loop through commands
        if isinstance(command, list):
            for cmd in command:
                if not self.run_ssh_command(hostname, cmd, verbose):
                    print(f"Command failed on host {hostname}: {cmd}", file=sys.stderr)
                    return False

            return True

        ssh_args = [
            "ssh",
            "-i", str(self.private_key),
            "-o", "IdentitiesOnly=yes",
            f"{self.user}@{hostname}",
            command,
        ]

        try:
            result = subprocess.run(
                ssh_args,
                check=True,
                capture_output=not verbose,
                text=True,
            )
            return result.returncode == 0
        except subprocess.CalledProcessError as e:
            print(f"\nSSH command failed with exit code {e.returncode}")
            print(f"Command: {command}")
            if e.stdout:
                print(f"stdout: {e.stdout}")
            if e.stderr:
                print(f"stderr: {e.stderr}")
            return False

    @abstractmethod
    def deploy_to_host(self, hostname: str, verbose: bool = False) -> bool:
        """
        Deploy to a single host.

        Args:
            hostname: Target hostname
            verbose: Show verbose output

        Returns:
            True if successful, False otherwise
        """
        pass

    def deploy_all(self, verbose: bool = False) -> None:
        """
        Deploy to all configured hosts.

        Args:
            verbose: Show verbose output
        """
        for hostname in self.hostnames:
            self.deploy_to_host(hostname, verbose=verbose)
