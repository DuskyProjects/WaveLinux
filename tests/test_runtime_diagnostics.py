import os
import tempfile
import unittest

from audio_runtime.diagnostics import RuntimeDiagnostics


class RuntimeDiagnosticsTests(unittest.TestCase):
    def test_constructor_prunes_old_failure_exports(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            for idx in range(4):
                path = os.path.join(tmpdir, f"runtime-failure-20260101-00000{idx}.json")
                with open(path, "w", encoding="utf-8") as handle:
                    handle.write("{}")
                os.utime(path, (idx + 1, idx + 1))

            RuntimeDiagnostics(root_dir=tmpdir, max_exports=2)

            kept = sorted(name for name in os.listdir(tmpdir) if name.endswith(".json"))
            self.assertEqual(len(kept), 2)
            self.assertEqual(
                kept,
                [
                    "runtime-failure-20260101-000002.json",
                    "runtime-failure-20260101-000003.json",
                ],
            )

    def test_export_failure_keeps_bounded_history(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            diag = RuntimeDiagnostics(root_dir=tmpdir, max_exports=2)
            first = diag.export_failure("one")
            second = diag.export_failure("two")
            third = diag.export_failure("three")

            kept = sorted(name for name in os.listdir(tmpdir) if name.endswith(".json"))
            self.assertEqual(len(kept), 2)
            self.assertFalse(os.path.exists(first))
            self.assertTrue(os.path.exists(second))
            self.assertTrue(os.path.exists(third))


if __name__ == "__main__":
    unittest.main()
