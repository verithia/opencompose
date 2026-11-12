"""Exercise the third-party license policy and notice rendering."""

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "third-party-licenses.py"
spec = importlib.util.spec_from_file_location("third_party_licenses", SCRIPT)
assert spec is not None and spec.loader is not None
notices = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = notices
spec.loader.exec_module(notices)

ALLOWED = notices.PERMISSIVE | notices.APPLICATION_ONLY


def package(name, license, files=None, authors=()):
    directory = Path(tempfile.mkdtemp(prefix="oc-notice-"))
    for file_name, text in (files or {}).items():
        (directory / file_name).write_text(text, encoding="utf-8")
    return notices.Package("crate", name, "1.0.0", license, list(authors), directory)


class ExpressionTests(unittest.TestCase):
    def test_a_slash_reads_as_or_and_prefers_mit(self):
        self.assertEqual(notices.chosen("Apache-2.0/MIT", ALLOWED), frozenset({"MIT"}))

    def test_and_requires_every_term(self):
        self.assertEqual(
            notices.chosen("(MIT OR Apache-2.0) AND Unicode-3.0", ALLOWED),
            frozenset({"MIT", "Unicode-3.0"}),
        )

    def test_with_exceptions_stay_one_term(self):
        self.assertEqual(
            notices.chosen("Apache-2.0 WITH LLVM-exception", ALLOWED),
            frozenset({"Apache-2.0 WITH LLVM-exception"}),
        )

    def test_a_permissive_alternative_avoids_lgpl(self):
        self.assertEqual(notices.chosen("MIT OR Apache-2.0 OR LGPL-2.1-or-later", ALLOWED), frozenset({"MIT"}))

    def test_an_unbalanced_expression_is_refused(self):
        with self.assertRaises(notices.PolicyError):
            notices.chosen("(MIT OR Apache-2.0", ALLOWED)


class PolicyTests(unittest.TestCase):
    def test_gpl_is_refused_anywhere(self):
        errors = notices.policy_errors([package("copyleft", "GPL-3.0-only")], [])
        self.assertEqual(len(errors), 1)
        self.assertIn("copyleft", errors[0])

    def test_mpl_is_allowed_in_the_application_only(self):
        codec = package("codec", "MPL-2.0")
        self.assertEqual(notices.policy_errors([codec], []), [])
        errors = notices.policy_errors([codec], [codec])
        self.assertEqual(len(errors), 1)
        self.assertIn("not permissive", errors[0])


class RenderTests(unittest.TestCase):
    def test_identical_notices_are_written_once(self):
        mit = "MIT License\n\nCopyright (c) Someone\n"
        text = notices.render([package("a", "MIT", {"LICENSE": mit}), package("b", "MIT", {"LICENSE": mit})])
        self.assertEqual(text.count("Copyright (c) Someone"), 1)
        self.assertIn("crate a 1.0.0 (MIT): [1]", text)
        self.assertIn("crate b 1.0.0 (MIT): [1]", text)

    def test_an_apache_copy_keeps_its_copyright_and_points_to_the_shared_text(self):
        apache = (notices.STANDARD_TEXTS / "Apache-2.0.txt").read_text(encoding="utf-8")
        apache += "\nCopyright 2024 Example Authors\n"
        text = notices.render([package("a", "Apache-2.0", {"LICENSE-APACHE": apache})])
        self.assertIn("Copyright 2024 Example Authors", text)
        self.assertIn("see [Apache-2.0] below", text)
        self.assertEqual(text.count("TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION"), 1)

    def test_a_package_without_files_gets_the_standard_text_with_its_authors(self):
        text = notices.render([package("bare", "MIT", authors=["Jane Doe"])])
        self.assertIn("Copyright (c) Jane Doe", text)
        self.assertIn("standard text; the package ships no license file", text)

    def test_a_missing_standard_text_is_an_error(self):
        with self.assertRaises(notices.PolicyError):
            notices.render([package("bare", "ISC")])


if __name__ == "__main__":
    unittest.main()
