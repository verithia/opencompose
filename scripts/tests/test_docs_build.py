"""Exercise the book build's check for the menu bar the language switch needs."""

import os
import shutil
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# Stand-ins for mdbook and cargo: the check reads only the rendered pages, so
# a fake that writes one page per book keeps the test free of either tool.
FAKE_MDBOOK = """#!/usr/bin/env bash
set -euo pipefail
[ "$1" = build ] || exit 2
if [ "${2:-}" = docs-zh ]; then out=book/zh; else rm -rf book; out=book; fi
mkdir -p "$out"
printf '%s' "$FAKE_PAGE" > "$out/index.html"
"""
FAKE_CARGO = """#!/usr/bin/env bash
mkdir -p target/doc/oc_dsl && echo '<html></html>' > target/doc/oc_dsl/index.html
"""
MENU_BAR = '<div id="mdbook-menu-bar"><div class="right-buttons"></div></div>'


class BookThemeHookTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="opencompose-docs-build-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "scripts").mkdir()
        shutil.copy2(ROOT / "scripts/docs.sh", self.root / "scripts/docs.sh")
        (self.root / "plugin-index").mkdir()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        for name, body in (("mdbook", FAKE_MDBOOK), ("cargo", FAKE_CARGO)):
            tool = self.bin / name
            tool.write_text(body)
            tool.chmod(tool.stat().st_mode | stat.S_IXUSR)

    def build(self, page):
        env = dict(os.environ, PATH=f"{self.bin}{os.pathsep}{os.environ['PATH']}", FAKE_PAGE=page)
        return subprocess.run(
            ["bash", str(self.root / "scripts/docs.sh"), "build"],
            cwd=self.root,
            env=env,
            capture_output=True,
            text=True,
        )

    def test_a_book_with_the_menu_bar_builds(self):
        result = self.build(f"<html>{MENU_BAR}</html>")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_a_book_without_the_menu_bar_is_refused(self):
        result = self.build('<html><div id="menu-bar"><div class="right-buttons"></div></div></html>')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("mdbook-menu-bar", result.stderr)

    def test_the_switch_targets_the_checked_menu_bar(self):
        script = (ROOT / "docs/theme/lang-switch.js").read_text()
        self.assertIn('"#mdbook-menu-bar .right-buttons"', script)


if __name__ == "__main__":
    unittest.main()
