import os
import tempfile
import unittest

from app_core.feature_flags import load_feature_flags


class FeatureFlagsTests(unittest.TestCase):
    def test_load_feature_flags_merges_env_and_json(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            path = os.path.join(temp_dir, "debug-modules.json")
            with open(path, "w", encoding="utf-8") as handle:
                handle.write(
                    '{"disabled_modules":["health"],'
                    '"start_disabled_modules":["scenes_module"],'
                    '"force_restart_modules":["updates"]}'
                )

            flags = load_feature_flags(
                environ={
                    "WAVELINUX_DISABLE_MODULES": "effects,metering",
                    "WAVELINUX_START_DISABLED_MODULES": "app_routing",
                    "WAVELINUX_FORCE_MODULE_RESTARTS": "device_policy",
                },
                path=path,
            )

        self.assertEqual(flags.disabled_modules, {"effects", "metering", "health"})
        self.assertEqual(flags.start_disabled_modules, {"app_routing", "scenes"})
        self.assertEqual(flags.force_restart_modules, {"device_policy", "updates"})
        self.assertTrue(flags.is_disabled("effects"))
        self.assertTrue(flags.is_disabled("scenes"))


if __name__ == "__main__":
    unittest.main()
