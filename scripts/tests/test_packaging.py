"""Keep the packaged bundles within their distribution and size policy."""

import json
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TAURI = ROOT / "crates/oc-gui/frontend/src-tauri"


class ProjectFileAssociationTests(unittest.TestCase):
    def setUp(self):
        self.config = json.loads((TAURI / "tauri.conf.json").read_text())
        self.bundle = self.config["bundle"]
        (self.association,) = [item for item in self.bundle["fileAssociations"] if "ocp" in item["ext"]]

    def test_macos_exports_its_project_document_type(self):
        self.assertEqual(self.association["role"], "Editor")
        self.assertEqual(self.association["rank"], "Owner")
        exported = self.association["exportedType"]
        self.assertEqual(exported["identifier"], f"{self.config['identifier']}.project")
        self.assertEqual(set(exported["conformsTo"]), {"public.data", "public.content"})

    def test_windows_uses_the_shared_installer_association(self):
        override = json.loads((TAURI / "tauri.windows.conf.json").read_text())
        self.assertNotIn("fileAssociations", override.get("bundle", {}))
        self.assertEqual(self.association["name"], "OpenCompose Project")
        self.assertEqual(self.association["ext"], ["ocp"])

    def test_linux_bundle_payloads_map_ocp_to_the_launcher_mime_type(self):
        linux = self.bundle["linux"]
        destination = f"/usr/share/mime/packages/{self.config['identifier']}.xml"
        namespace = {"m": "http://www.freedesktop.org/standards/shared-mime-info"}
        for target in ("deb", "appimage"):
            with self.subTest(target=target):
                source = TAURI / linux[target]["files"][destination]
                root = ET.parse(source).getroot()
                entry = root.find("m:mime-type", namespace)
                self.assertEqual(entry.attrib["type"], self.association["mimeType"])
                self.assertIn({"pattern": "*.ocp"}, [glob.attrib for glob in entry.findall("m:glob", namespace)])
        self.assertIn("shared-mime-info", linux["deb"]["depends"])

    def test_linux_launcher_passes_one_file_to_the_existing_open_handler(self):
        template = (TAURI / self.bundle["linux"]["deb"]["desktopTemplate"]).read_text()
        self.assertIn("Exec={{exec}} %f", template.splitlines())
        self.assertIn("MimeType={{mime_type}};", template.splitlines())


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


class ReleaseWorkflowTests(unittest.TestCase):
    def workflow(self, name):
        return (ROOT / ".github/workflows" / name).read_text()

    def test_release_bundles_come_from_the_packaging_workflow(self):
        release = self.workflow("release.yml")
        package = self.workflow("package.yml")
        self.assertIn("uses: ./.github/workflows/package.yml\n    with:\n      tag: ${{ inputs.tag }}", release)
        self.assertIn("  workflow_call:\n", package)
        self.assertIn("ref: ${{ inputs.tag || github.ref }}", package)

    def test_the_draft_waits_for_the_served_index_and_the_bundles(self):
        release = self.workflow("release.yml")
        self.assertIn("permissions:\n  contents: read\n", release)
        draft = release.split("\n  draft:\n", 1)[1]
        self.assertIn("needs: [index, package]", draft)
        self.assertIn("permissions:\n      contents: write", draft)
        self.assertIn("--draft", draft)
        self.assertIn("--verify-tag", draft)
        index = release.split("\n  index:\n", 1)[1].split("\n  package:\n", 1)[0]
        self.assertIn("for file in index.json index.json.sig; do", index)
        self.assertIn('cmp -s "served-$file" "plugin-index/$file"', index)

    def test_the_version_gate_covers_tag_pushes_and_release_calls(self):
        package = self.workflow("package.yml")
        tag_line = next(line for line in package.splitlines() if line.strip().startswith("RELEASE_TAG:"))
        self.assertIn("inputs.tag", tag_line)
        self.assertIn("refs/tags/", tag_line)
        gate = package.split("      - name: Verify the tag matches the package version\n", 1)[1].split(
            "\n      - name:", 1
        )[0]
        self.assertIn("if: env.RELEASE_TAG != ''", gate)


if __name__ == "__main__":
    unittest.main()
