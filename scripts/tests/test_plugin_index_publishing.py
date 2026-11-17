"""Keep the committed plugin index wired to the site that serves it."""

import re
import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def base_urls():
    source = (ROOT / "crates/oc-market/src/index/verified.rs").read_text()
    return {
        name: re.search(name + r':\s*&str\s*=\s*"([^"]+)"', source).group(1)
        for name in ("PRIMARY_BASE_URL", "FALLBACK_BASE_URL")
    }


class PluginIndexPublishingTests(unittest.TestCase):
    def test_origins_serve_the_committed_directory(self):
        urls = base_urls()
        self.assertTrue(urls["PRIMARY_BASE_URL"].endswith("/plugin-index/"), urls)
        self.assertTrue(urls["FALLBACK_BASE_URL"].endswith("/main/plugin-index/"), urls)
        self.assertTrue(all(url.startswith("https://") for url in urls.values()), urls)

    def test_index_changes_deploy_the_site(self):
        workflow = (ROOT / ".github/workflows/docs.yml").read_text()
        self.assertIn("- 'plugin-index/**'", workflow)

    def test_site_build_copies_the_index_after_the_book(self):
        script = (ROOT / "scripts/docs.sh").read_text()
        build = script.split("    build)\n", 1)[1].split(";;", 1)[0].split()
        self.assertEqual(build, ["build_book", "build_api", "copy_plugin_index"])
        self.assertIn("cp -R plugin-index book/plugin-index", script)

    def test_signed_files_are_checked_out_byte_for_byte(self):
        for name in ("index.json", "index.json.sig"):
            attributes = subprocess.run(
                ["git", "check-attr", "text", "diff", "--", f"plugin-index/{name}"],
                cwd=ROOT,
                capture_output=True,
                text=True,
                check=True,
            ).stdout
            self.assertIn(": text: unset", attributes)
            self.assertIn(": diff: unset", attributes)


if __name__ == "__main__":
    unittest.main()
