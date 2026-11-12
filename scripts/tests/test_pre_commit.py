"""Exercise the formatting hook against real, disposable Git indexes."""

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

HOOK = Path(__file__).resolve().parents[2] / ".githooks" / "pre-commit"


class FormattingHookTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        for tool in ("git", "rustfmt", "ruff"):
            if shutil.which(tool) is None:
                raise RuntimeError(f"Hook verification requires {tool}")

    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="opencompose-hook-")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.env = {**os.environ, "GIT_CONFIG_GLOBAL": os.devnull}
        self.git("-c", "init.templateDir=", "init", "--quiet")
        self.git("config", "core.autocrlf", "false")

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root, env=self.env, text=True)

    def stage(self, name, content):
        path = self.root / name
        path.write_text(content)
        self.git("add", "--", name)
        return path

    def run_hook(self):
        return subprocess.run(
            ["sh", str(HOOK)],
            cwd=self.root,
            env=self.env,
            capture_output=True,
            text=True,
            check=False,
        )

    def assert_success(self, result):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_empty_index_needs_no_formatting(self):
        self.assert_success(self.run_hook())

    def test_fully_staged_rust_formats_only_its_own_path(self):
        for name in ("staged module.rs", "音色\nmodule.rs"):
            with self.subTest(name=name):
                path = self.stage(name, "fn main( ) {}\n")
                untouched = self.root / "unstaged.rs"
                untouched.write_text("fn untouched( ) {}\n")
                self.assert_success(self.run_hook())
                self.assertEqual(path.read_text(), "fn main() {}\n")
                self.assertEqual(self.git("show", f":{name}"), path.read_text())
                self.assertEqual(untouched.read_text(), "fn untouched( ) {}\n")

    def test_partial_rust_checks_index_without_staging_or_formatting_work(self):
        staged = "fn main() {}\n"
        path = self.stage("partial.rs", staged)
        working = staged + "fn unfinished( ) {}\n"
        path.write_text(working)
        self.assert_success(self.run_hook())
        self.assertEqual(self.git("show", ":partial.rs"), staged)
        self.assertEqual(path.read_text(), working)

    def test_unformatted_partial_rust_refuses_without_modifying_either_copy(self):
        staged = "fn main( ) {}\n"
        path = self.stage("partial.rs", staged)
        working = staged + "// Work outside the commit.\n"
        path.write_text(working)
        self.assertNotEqual(self.run_hook().returncode, 0)
        self.assertEqual(self.git("show", ":partial.rs"), staged)
        self.assertEqual(path.read_text(), working)

    def test_python_formats_staged_file_and_preserves_partial_work(self):
        full = self.stage("staged module.py", "import sys\nimport os\n\nprint(sys.version,os.name)\n")
        staged = 'print("staged")\n'
        partial = self.stage("partial.py", staged)
        working = staged + 'print(  "unfinished" )\n'
        partial.write_text(working)
        self.assert_success(self.run_hook())
        self.assertEqual(full.read_text(), "import os\nimport sys\n\nprint(sys.version, os.name)\n")
        self.assertEqual(self.git("show", ":staged module.py"), full.read_text())
        self.assertEqual(self.git("show", ":partial.py"), staged)
        self.assertEqual(partial.read_text(), working)

    def test_unformatted_partial_python_refuses_without_changing_either_copy(self):
        staged = 'print(  "staged" )\n'
        path = self.stage("partial.py", staged)
        working = staged + "# Work outside the commit.\n"
        path.write_text(working)
        self.assertNotEqual(self.run_hook().returncode, 0)
        self.assertEqual(self.git("show", ":partial.py"), staged)
        self.assertEqual(path.read_text(), working)

    @unittest.skipIf(os.name == "nt", "Symlink creation may require Windows privileges")
    def test_staged_symlink_never_formats_its_target(self):
        outside = self.root.parent / (self.root.name + "-outside.rs")
        outside.write_text("fn untouched( ) {}\n")
        self.addCleanup(outside.unlink)
        (self.root / "alias.rs").symlink_to(outside)
        self.git("add", "alias.rs")
        self.assert_success(self.run_hook())
        self.assertEqual(outside.read_text(), "fn untouched( ) {}\n")


if __name__ == "__main__":
    unittest.main()
