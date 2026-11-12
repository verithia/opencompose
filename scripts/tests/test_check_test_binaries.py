"""Exercise the integration-test binary budget checker."""

import contextlib
import importlib.util
import io
import unittest
from pathlib import Path
from unittest import mock

SCRIPT = Path(__file__).resolve().parents[1] / "check-test-binaries.py"
spec = importlib.util.spec_from_file_location("check_test_binaries", SCRIPT)
assert spec is not None and spec.loader is not None
budget = importlib.util.module_from_spec(spec)
spec.loader.exec_module(budget)


class TestBinaryBudgetTests(unittest.TestCase):
    def run_check(self, targets):
        out = io.StringIO()
        err = io.StringIO()
        with mock.patch.object(budget, "targets", return_value=targets):
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                code = budget.main()
        return code, out.getvalue(), err.getvalue()

    def test_one_aggregate_binary_passes(self):
        code, out, err = self.run_check({"oc-engine": ["all"], "oc-dsp": ["all"]})
        self.assertEqual(code, 0, err)
        self.assertIn("within budget", out)

    def test_an_unexpected_extra_binary_fails(self):
        code, out, err = self.run_check({"oc-engine": ["all", "one_off"], "oc-dsp": ["all"]})
        self.assertEqual(code, 1)
        self.assertIn("oc-engine", err)
        self.assertIn("one_off", err)

    def test_allowlisted_harnesses_pass(self):
        code, out, err = self.run_check({"oc-synth": ["all", "fx_parallel_alloc", "source_render_alloc"]})
        self.assertEqual(code, 0, err)
        self.assertIn("within budget", out)


if __name__ == "__main__":
    unittest.main()
