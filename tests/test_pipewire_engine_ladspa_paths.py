import os
import unittest
from unittest import mock

from pipewire_engine import PipeWireEngine


class PipeWireEngineLadspaPathTests(unittest.TestCase):
    def test_host_paths_precede_bundled_paths(self):
        with mock.patch.dict(
            os.environ,
            {
                "LADSPA_PATH": "/custom/ladspa",
                "WAVELINUX_BUNDLED_LADSPA_PATH": "/tmp/appdir/usr/lib/ladspa",
                "WAVELINUX_ENABLE_BUNDLED_LADSPA": "1",
            },
            clear=False,
        ):
            roots = PipeWireEngine._ladspa_roots()

        self.assertEqual(roots[0], "/custom/ladspa")
        self.assertEqual(roots[-1], "/tmp/appdir/usr/lib/ladspa")

    def test_bundled_paths_are_ignored_by_default(self):
        with mock.patch.dict(
            os.environ,
            {
                "LADSPA_PATH": "",
                "WAVELINUX_BUNDLED_LADSPA_PATH": "/tmp/appdir/usr/lib/ladspa",
                "WAVELINUX_ENABLE_BUNDLED_LADSPA": "",
            },
            clear=False,
        ):
            roots = PipeWireEngine._ladspa_roots()

        self.assertNotIn("/tmp/appdir/usr/lib/ladspa", roots)


if __name__ == "__main__":
    unittest.main()
