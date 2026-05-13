import unittest

from main import UpdateChecker


class UpdateCheckerTests(unittest.TestCase):
    def test_release_info_from_data_prefers_wavelinux_x86_64_appimage(self):
        data = {
            "tag_name": "v2.0.5",
            "html_url": "https://github.com/DuskyProjects/WaveLinux/releases/tag/v2.0.5",
            "assets": [
                {
                    "name": "WaveLinux-2.0.5-arm64.AppImage",
                    "browser_download_url": "https://example.test/arm64",
                },
                {
                    "name": "WaveLinux-2.0.5-x86_64.AppImage",
                    "browser_download_url": "https://example.test/x86_64",
                },
                {
                    "name": "source.tar.gz",
                    "browser_download_url": "https://example.test/source",
                },
            ],
        }

        info = UpdateChecker._release_info_from_data(data)

        self.assertEqual(info["tag"], "2.0.5")
        self.assertEqual(info["asset_name"], "WaveLinux-2.0.5-x86_64.AppImage")
        self.assertEqual(info["asset_url"], "https://example.test/x86_64")

    def test_release_info_from_data_returns_empty_asset_fields_when_missing(self):
        data = {
            "tag_name": "v2.0.5",
            "html_url": "https://github.com/DuskyProjects/WaveLinux/releases/tag/v2.0.5",
            "assets": [
                {
                    "name": "source.tar.gz",
                    "browser_download_url": "https://example.test/source",
                }
            ],
        }

        info = UpdateChecker._release_info_from_data(data)

        self.assertEqual(info["tag"], "2.0.5")
        self.assertEqual(info["asset_name"], "")
        self.assertEqual(info["asset_url"], "")

    def test_release_info_from_data_requires_tag(self):
        with self.assertRaisesRegex(ValueError, "no release tag"):
            UpdateChecker._release_info_from_data({"assets": []})


if __name__ == "__main__":
    unittest.main()
