"""
Tests for the scoped Home Assistant component deploy.

These exist because of ledger `shq-suite-0033`: `./setup ha -c dosa --restart`
accepted `-c` and then rsynced the ENTIRE `custom_components/` tree anyway. That
overwrote the live Argus integration (1.68.1, owned by the `jordonsc/argus` repo)
with a dead 8-file stub still carried here, HA failed to import it, and all ten
wall kiosks lost their dashboard for ~6 hours.

`test_scoped_deploy_leaves_sibling_components_untouched` is a literal replay of
that incident and is the test that must never be allowed to fail.

Run with:  python3 -m unittest discover -s deploy/tests -t deploy/tests
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))

from deploy.ha_deployer import HomeAssistantDeployer  # noqa: E402


def _write_component(root: Path, name: str, version: str, extra: str = "") -> Path:
    """Create a minimal custom component directory."""
    d = root / name
    d.mkdir(parents=True, exist_ok=True)
    (d / "manifest.json").write_text(json.dumps({"domain": name, "version": version}))
    (d / "__init__.py").write_text(f"# {name} {version}\n{extra}")
    return d


class HomeAssistantDeployerScopeTest(unittest.TestCase):
    """The rsync must be scoped to the components named with `-c`."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp, True)

        # Local repo tree: home-assistant/custom_components/{dosa,argus,somfy_sdn}
        self.source = self.tmp / "repo" / "home-assistant" / "custom_components"
        self.source.mkdir(parents=True)
        _write_component(self.source, "dosa", "2.0.0")
        _write_component(self.source, "somfy_sdn", "1.0.0")

        # Remote HA config root: /etc/hass equivalent
        self.dest = self.tmp / "etc" / "hass"
        (self.dest / "custom_components").mkdir(parents=True)

    def _deployer(self, components):
        return HomeAssistantDeployer(
            hostnames=["ha.test"],
            user="jordonsc",
            private_key="/dev/null",
            source_path=str(self.source),
            destination_path=str(self.dest),
            service_name="hass",
            components=components,
        )

    def test_scoped_deploy_resolves_only_the_named_component(self):
        pairs = self._deployer(["dosa"]).resolve_component_sources()
        self.assertEqual(
            pairs,
            [(str(self.source / "dosa"), str(self.dest / "custom_components"))],
        )

    def test_scoped_deploy_resolves_several_named_components(self):
        pairs = self._deployer(["dosa", "somfy_sdn"]).resolve_component_sources()
        self.assertEqual(
            [Path(s).name for s, _ in pairs],
            ["dosa", "somfy_sdn"],
        )
        for _, destination in pairs:
            self.assertEqual(destination, str(self.dest / "custom_components"))

    def test_scoped_deploy_rsyncs_only_the_named_component(self):
        """deploy_to_host must never hand rsync the whole tree when scoped."""
        deployer = self._deployer(["dosa"])
        calls = []

        with mock.patch.object(
            HomeAssistantDeployer, "run_rsync", autospec=True,
            side_effect=lambda self, src, dst, host, **kw: calls.append((str(src), str(dst))) or True,
        ), mock.patch.object(HomeAssistantDeployer, "run_ssh_command", return_value=True), \
                mock.patch.object(HomeAssistantDeployer, "_reload_ha_config", return_value=True):
            self.assertTrue(deployer.deploy_to_host("ha.test"))

        component_calls = [c for c in calls if "custom_components" in c[0]]
        self.assertEqual(
            component_calls,
            [(str(self.source / "dosa"), str(self.dest / "custom_components"))],
        )
        # The bare tree root must never be a source.
        self.assertNotIn(str(self.source), [c[0] for c in calls])

    def test_unscoped_deploy_still_syncs_the_whole_tree(self):
        """--all-components (components=None) keeps the legacy behaviour."""
        pairs = self._deployer(None).resolve_component_sources()
        self.assertEqual(pairs, [(str(self.source), str(self.dest))])

    def test_unknown_component_fails_before_any_rsync(self):
        """A typo must fail locally, never push a partial tree."""
        deployer = self._deployer(["dsoa"])  # transposed
        with self.assertRaises(FileNotFoundError):
            deployer.resolve_component_sources()

        with mock.patch.object(HomeAssistantDeployer, "run_rsync", autospec=True) as rsync, \
                mock.patch.object(HomeAssistantDeployer, "run_ssh_command", return_value=True):
            self.assertFalse(deployer.deploy_to_host("ha.test"))
        rsync.assert_not_called()


class ShqSuite0033RegressionTest(unittest.TestCase):
    """A literal replay of the incident, driving real rsync over real files."""

    def setUp(self):
        if shutil.which("rsync") is None:
            self.skipTest("rsync not available")
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp, True)

        # This repo still carries a stale `argus` stub alongside the component
        # we actually want to deploy. (The stub is now deleted from shq-suite;
        # the test keeps the hazard alive so the scoping cannot silently regress.)
        self.source = self.tmp / "repo" / "home-assistant" / "custom_components"
        self.source.mkdir(parents=True)
        _write_component(self.source, "dosa", "2.0.0")
        _write_component(self.source, "argus", "0.1.0")  # the dead stub

        # The live host runs the REAL Argus integration, owned by another repo.
        self.dest = self.tmp / "etc" / "hass"
        remote_cc = self.dest / "custom_components"
        remote_cc.mkdir(parents=True)
        _write_component(remote_cc, "dosa", "1.0.0")
        _write_component(
            remote_cc, "argus", "1.68.1", extra="CONF_ID = 'id'\n"
        )
        (remote_cc / "argus" / "frontend").mkdir()
        (remote_cc / "argus" / "frontend" / "argus-kit.js").write_text("// the kiosk bundle\n")

        # Age the remote tree. rsync -a's quick check is size+mtime, and the
        # fixtures are written milliseconds apart at identical sizes, so without
        # this an update is silently skipped. (This is the same mtime-preserving
        # property that hid the real clobber: only ctime showed it.)
        old = 1_600_000_000
        for path in remote_cc.rglob("*"):
            os.utime(path, (old, old))

    def _rsync(self, source: str, destination: str) -> None:
        """Local stand-in for run_rsync, with the same flags."""
        subprocess.run(
            ["rsync", "-a", "--quiet", "--exclude", "__pycache__", "--exclude", "*.pyc",
             source, destination],
            check=True,
        )

    def _remote_argus_manifest(self) -> dict:
        return json.loads(
            (self.dest / "custom_components" / "argus" / "manifest.json").read_text()
        )

    def test_scoped_deploy_leaves_sibling_components_untouched(self):
        """Deploying `dosa` must not touch the live `argus` integration."""
        deployer = HomeAssistantDeployer(
            hostnames=["ha.test"], user="jordonsc", private_key="/dev/null",
            source_path=str(self.source), destination_path=str(self.dest),
            service_name="hass", components=["dosa"],
        )
        for source, destination in deployer.resolve_component_sources():
            self._rsync(source, destination)

        # dosa was updated...
        self.assertEqual(
            json.loads((self.dest / "custom_components" / "dosa" / "manifest.json").read_text())["version"],
            "2.0.0",
        )
        # ...and argus was NOT reverted to the stub. This is shq-suite-0033.
        self.assertEqual(self._remote_argus_manifest()["version"], "1.68.1")
        self.assertIn(
            "CONF_ID",
            (self.dest / "custom_components" / "argus" / "__init__.py").read_text(),
        )
        self.assertTrue(
            (self.dest / "custom_components" / "argus" / "frontend" / "argus-kit.js").exists(),
            "the kiosk bundle must survive an unrelated component deploy",
        )

    def test_unscoped_deploy_is_what_broke_the_estate(self):
        """Proves the whole-tree sync really does clobber — hence the opt-in gate."""
        deployer = HomeAssistantDeployer(
            hostnames=["ha.test"], user="jordonsc", private_key="/dev/null",
            source_path=str(self.source), destination_path=str(self.dest),
            service_name="hass", components=None,
        )
        for source, destination in deployer.resolve_component_sources():
            self._rsync(source, destination)

        self.assertEqual(
            self._remote_argus_manifest()["version"], "0.1.0",
            "the unscoped sync is destructive; it must stay behind --all-components",
        )


class HaCommandScopeGuardTest(unittest.TestCase):
    """`./setup ha` must not sync anything unless the scope is stated."""

    @classmethod
    def setUpClass(cls):
        try:
            from click.testing import CliRunner  # noqa: F401
        except ImportError:  # pragma: no cover
            raise unittest.SkipTest("click not available")

        import importlib.util

        src = Path(__file__).resolve().parent.parent / "src"
        spec = importlib.util.spec_from_file_location("deploy_cli", src / "deploy.py")
        cls.cli_module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.cli_module)

    def _run(self, args):
        from click.testing import CliRunner

        fake_config = mock.Mock(
            hostnames=["ha.test"], user="jordonsc", private_key="/dev/null",
            source_path="/src/custom_components", component_path="/etc/hass",
            systemd_service="hass",
        )
        with mock.patch.object(
            self.cli_module.ConfigPresets, "get_ha_config", return_value=fake_config
        ), mock.patch.object(self.cli_module, "HomeAssistantDeployer") as deployer:
            result = CliRunner().invoke(self.cli_module.ha, args)
        return result, deployer

    def test_no_scope_is_refused(self):
        """The bare `./setup ha` must not silently sync the whole tree."""
        result, deployer = self._run([])
        self.assertNotEqual(result.exit_code, 0)
        self.assertIn("No component selected", result.output)
        deployer.assert_not_called()

    def test_scope_and_all_components_are_mutually_exclusive(self):
        result, deployer = self._run(["-c", "dosa", "--all-components"])
        self.assertNotEqual(result.exit_code, 0)
        self.assertIn("mutually exclusive", result.output)
        deployer.assert_not_called()

    def test_named_components_reach_the_deployer(self):
        result, deployer = self._run(["-c", "dosa", "-c", "somfy_sdn"])
        self.assertEqual(result.exit_code, 0, result.output)
        self.assertEqual(
            deployer.call_args.kwargs["components"], ["dosa", "somfy_sdn"]
        )

    def test_all_components_opts_in_to_the_whole_tree(self):
        result, deployer = self._run(["--all-components"])
        self.assertEqual(result.exit_code, 0, result.output)
        self.assertIsNone(deployer.call_args.kwargs["components"])


if __name__ == "__main__":
    unittest.main()
