"""Keep the effective Windows distribution within its download-on-demand policy."""

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


if __name__ == "__main__":
    unittest.main()
