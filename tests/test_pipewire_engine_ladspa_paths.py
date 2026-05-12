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

    def test_appimage_injected_ladspa_path_is_ignored_by_default(self):
        with mock.patch.dict(
            os.environ,
            {
                "LADSPA_PATH": "/tmp/appdir/usr/lib/ladspa:/custom/ladspa",
                "WAVELINUX_BUNDLED_LADSPA_PATH": "/tmp/appdir/usr/lib/ladspa",
                "WAVELINUX_ENABLE_BUNDLED_LADSPA": "",
            },
            clear=False,
        ):
            roots = PipeWireEngine._ladspa_roots()
            spawn_env = PipeWireEngine._pipewire_spawn_env()

        self.assertEqual(roots[0], "/custom/ladspa")
        self.assertNotIn("/tmp/appdir/usr/lib/ladspa", roots)
        self.assertEqual(spawn_env.get("LADSPA_PATH"), "/custom/ladspa")

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

    def test_spawn_env_unsets_ladspa_path_when_only_bundle_is_present(self):
        with mock.patch.dict(
            os.environ,
            {
                "LADSPA_PATH": "/tmp/appdir/usr/lib/ladspa",
                "WAVELINUX_BUNDLED_LADSPA_PATH": "/tmp/appdir/usr/lib/ladspa",
                "WAVELINUX_ENABLE_BUNDLED_LADSPA": "",
            },
            clear=False,
        ):
            spawn_env = PipeWireEngine._pipewire_spawn_env()

        self.assertNotIn("LADSPA_PATH", spawn_env)


if __name__ == "__main__":
    unittest.main()
