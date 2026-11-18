"""Keep the packaged bundles within their distribution and size policy."""

import json
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TAURI = ROOT / "crates/oc-gui/frontend/src-tauri"


class WindowsPackagingTests(unittest.TestCase):
    def test_default_installer_downloads_the_runtime_only_when_missing(self):
        config = json.loads((TAURI / "tauri.conf.json").read_text())
        mode = config["bundle"]["windows"]["webviewInstallMode"]
        self.assertEqual(mode["type"], "downloadBootstrapper")

    def test_windows_override_preserves_the_runtime_distribution_policy(self):
        config = json.loads((TAURI / "tauri.windows.conf.json").read_text())
        windows = config.get("bundle", {}).get("windows", {})
        self.assertNotIn("webviewInstallMode", windows)

    def test_bundle_command_is_portable_to_the_windows_default_shell(self):
        workflow = (ROOT / ".github/workflows/package.yml").read_text()
        step = workflow.split("      - name: Build the bundle\n", 1)[1].split("\n      - name:", 1)[0]
        command = next(
            line.strip().removeprefix("run: ") for line in step.splitlines() if line.strip().startswith("run: ")
        )
        self.assertTrue(command.startswith("npm run tauri build -- "), command)
        self.assertIn("--target ${{ matrix.target }}", command)
        self.assertIn("--bundles ${{ matrix.bundles }}", command)
        self.assertTrue(command.endswith("-- --locked"), command)
        self.assertNotIn("\\", command)


class BundleBudgetTests(unittest.TestCase):
    def budget_step(self):
        workflow = (ROOT / ".github/workflows/package.yml").read_text()
        return workflow.split("      - name: Check the bundle size budgets\n", 1)[1].split("\n      - name:", 1)[0]

    def test_every_downloadable_format_is_held_to_100_mib(self):
        step = self.budget_step()
        downloads = step.split("downloads=(", 1)[1].split(")", 1)[0]
        for pattern in ("dmg/*.dmg", "deb/*.deb", "msi/*.msi", "nsis/*.exe"):
            self.assertIn(pattern, downloads)
        self.assertIn('check-bundle-size.sh --limit-mib 100 "${downloads[@]}"', step)

    def test_self_contained_bundles_are_held_to_150_mib(self):
        step = self.budget_step()
        self_contained = step.split("self_contained=(", 1)[1].split(")", 1)[0]
        self.assertIn("macos/*.app", self_contained)
        self.assertIn("appimage/*.AppImage", self_contained)
        self.assertIn('check-bundle-size.sh --limit-mib 150 "${self_contained[@]}"', step)

    def run_check(self, *args):
        import subprocess
        import tempfile

        script = ROOT / "scripts/check-bundle-size.sh"
        with tempfile.TemporaryDirectory() as folder:
            artifact = Path(folder) / "artifact.bin"
            artifact.write_bytes(b"\1" * 1024 * 1024 * 2)
            env = {"PATH": "/usr/bin:/bin"}
            return subprocess.run(["bash", str(script), *args, str(artifact)], env=env, capture_output=True)

    def test_the_limit_flag_sets_the_enforced_size(self):
        self.assertEqual(self.run_check("--limit-mib", "1").returncode, 1)
        self.assertEqual(self.run_check("--limit-mib", "3").returncode, 0)

    def test_a_malformed_limit_is_a_usage_error(self):
        self.assertEqual(self.run_check("--limit-mib", "0").returncode, 2)
        self.assertEqual(self.run_check("--limit-mib", "x").returncode, 2)


if __name__ == "__main__":
    unittest.main()
