"""Exercise the published-book tool gate against the real identity table."""

import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
IDENTITIES = Path("crates/oc-gui/frontend/src-tauri/src/tool_registry/identity.rs")


class BookToolGateTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="opencompose-doc-gate-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "scripts").mkdir()
        shutil.copy2(ROOT / "scripts/check-docs.sh", self.root / "scripts/check-docs.sh")
        self.identity = self.root / IDENTITIES
        self.identity.parent.mkdir(parents=True)
        shutil.copy2(ROOT / IDENTITIES, self.identity)
        (self.root / "docs/nested").mkdir(parents=True)
        (self.root / "docs/SUMMARY.md").write_text("# Summary\n- [Home](README.md)\n- [Guide](nested/guide.md)\n")
        (self.root / "docs/README.md").write_text("# Home\n`system_capabilities`\n")
        self.guide = self.root / "docs/nested/guide.md"
        self.guide.write_text("# Guide\n`tracks_list` and `sample_rate`.\n")
        shutil.copy2(ROOT / "docs/tool-names-allow.txt", self.root / "docs/tool-names-allow.txt")

    def check_book(self):
        return subprocess.run(
            ["bash", str(self.root / "scripts/check-docs.sh"), "--no-snippets"],
            capture_output=True,
            text=True,
            check=False,
        )

    def test_known_tools_and_documented_non_tool_identifiers_pass(self):
        result = self.check_book()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_unknown_tool_in_any_book_chapter_is_refused(self):
        self.guide.write_text("# Guide\nUse `tracks_nonexistent_operation`.\n")
        result = self.check_book()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("docs/nested/guide.md", result.stderr)
        self.assertIn("tracks_nonexistent_operation", result.stderr)

    def test_hidden_dispatch_identity_is_not_advertised_by_book(self):
        self.identity.write_text(
            self.identity.read_text().replace("plugin_index_status", "plugin_hidden_fixture_status")
        )
        self.guide.write_text("# Guide\nCall `plugin_hidden_fixture_status`.\n")
        result = self.check_book()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("plugin_hidden_fixture_status", result.stderr)

    def test_missing_identity_authority_fails_instead_of_using_copies(self):
        self.identity.unlink()
        result = self.check_book()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("extraction is broken", result.stderr)

    def test_multiline_identity_declaration_keeps_its_tool(self):
        source = self.identity.read_text()
        needle = 'SystemCapabilities => ("system_capabilities",'
        self.assertIn(needle, source)
        self.identity.write_text(source.replace(needle, 'SystemCapabilities => (\n    "system_capabilities",'))
        result = self.check_book()
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
