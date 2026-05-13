import io
import unittest
from contextlib import redirect_stdout
from types import SimpleNamespace
from unittest import mock

import main


class MainCliTests(unittest.TestCase):
    def test_handle_cli_args_prints_version(self):
        buf = io.StringIO()
        with redirect_stdout(buf):
            exit_code = main._handle_cli_args(["--version"])
        self.assertEqual(exit_code, 0)
        self.assertEqual(buf.getvalue().strip(), main.APP_VERSION)

    def test_handle_cli_args_prints_self_test_json(self):
        buf = io.StringIO()
        with mock.patch.object(main, "build_self_test_report", return_value={"ok": True, "version": main.APP_VERSION}):
            with redirect_stdout(buf):
                exit_code = main._handle_cli_args(["--self-test"])
        self.assertEqual(exit_code, 0)
        self.assertIn('"ok": true', buf.getvalue())

    def test_startup_preflight_report_captures_missing_and_failed_checks(self):
        def fake_which(name):
            if name == "parec":
                return None
            return f"/usr/bin/{name}"

        def fake_runner(cmd, capture_output, text, timeout):
            if cmd[:2] == ["pactl", "info"]:
                return SimpleNamespace(returncode=1, stderr="Connection refused")
            return SimpleNamespace(returncode=0, stderr="")

        report = main.startup_preflight_report(
            which=fake_which,
            runner=fake_runner,
            config_dir="/tmp/wavelinux-test-config",
        )

        self.assertIn("parec", report["missing"])
        self.assertIn(
            "Could not query the PulseAudio compatibility server via `pactl info`.",
            report["issues"],
        )
        self.assertTrue(report["config_ok"])


if __name__ == "__main__":
    unittest.main()
