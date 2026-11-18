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
        return workflow.split("      - name: Check the 100 MiB download budget\n", 1)[1].split("\n      - name:", 1)[0]

    def test_every_downloadable_format_is_enforced(self):
        enforced = self.budget_step().split("downloads=(", 1)[1].split(")", 1)[0]
        for pattern in ("dmg/*.dmg", "deb/*.deb", "msi/*.msi", "nsis/*.exe"):
            self.assertIn(pattern, enforced)

    def test_self_contained_bundles_are_reported_not_enforced(self):
        step = self.budget_step()
        reported = step.split("reported=(", 1)[1].split(")", 1)[0]
        self.assertIn("macos/*.app", reported)
        self.assertIn("appimage/*.AppImage", reported)
        self.assertIn('check-bundle-size.sh --report "${reported[@]}"', step)

    def test_report_mode_never_fails_on_size(self):
        import subprocess
        import tempfile

        script = ROOT / "scripts/check-bundle-size.sh"
        with tempfile.TemporaryDirectory() as folder:
            artifact = Path(folder) / "artifact.bin"
            artifact.write_bytes(b"\0" * 4096 * 4)
            env = {"BUNDLE_SIZE_LIMIT_KIB": "1", "PATH": "/usr/bin:/bin"}
            enforced = subprocess.run(["bash", str(script), str(artifact)], env=env, capture_output=True)
            reported = subprocess.run(["bash", str(script), "--report", str(artifact)], env=env, capture_output=True)
        self.assertEqual(enforced.returncode, 1)
        self.assertEqual(reported.returncode, 0)


if __name__ == "__main__":
    unittest.main()
