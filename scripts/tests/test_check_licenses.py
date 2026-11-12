"""Exercise the license boundary checker."""

import importlib.util
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "check-licenses.py"
spec = importlib.util.spec_from_file_location("check_licenses", SCRIPT)
assert spec is not None and spec.loader is not None
licenses = importlib.util.module_from_spec(spec)
spec.loader.exec_module(licenses)


def package(name, license, dependencies=()):
    return {"name": name, "license": license, "dependencies": [{"name": d} for d in dependencies]}


def workspace(**overrides):
    packages = {
        "oc-dsl": package("oc-dsl", licenses.PERMISSIVE, ["oc-device-schema", "oc-test-support", "serde"]),
        "oc-device-schema": package("oc-device-schema", licenses.PERMISSIVE, ["serde"]),
        "oc-lsp": package("oc-lsp", licenses.PERMISSIVE, ["oc-dsl"]),
        "oc-test-support": package("oc-test-support", licenses.PERMISSIVE),
        "oc-engine": package("oc-engine", licenses.APPLICATION, ["oc-dsl"]),
    }
    packages.update(overrides)
    return list(packages.values())


class LicenseBoundaryTests(unittest.TestCase):
    def test_the_intended_split_passes(self):
        self.assertEqual(licenses.violations(workspace()), [])

    def test_a_permissive_crate_depending_on_an_application_crate_fails(self):
        errors = licenses.violations(
            workspace(**{"oc-dsl": package("oc-dsl", licenses.PERMISSIVE, ["oc-device-schema", "oc-engine"])})
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("oc-dsl", errors[0])
        self.assertIn("oc-engine", errors[0])

    def test_an_application_crate_with_the_wrong_license_fails(self):
        errors = licenses.violations(workspace(**{"oc-engine": package("oc-engine", "MIT")}))
        self.assertEqual(len(errors), 1)
        self.assertIn("oc-engine", errors[0])

    def test_a_missing_permissive_crate_is_reported(self):
        packages = [p for p in workspace() if p["name"] != "oc-lsp"]
        errors = licenses.violations(packages)
        self.assertEqual(errors, ["oc-lsp: listed as permissive but not a workspace crate"])


if __name__ == "__main__":
    unittest.main()
