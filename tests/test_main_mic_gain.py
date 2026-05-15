import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt6.QtWidgets import QApplication

from main import ChannelStrip, MixerStripMetrics


class _FakeRuntime:
    def __init__(self):
        self.source_volume_calls = []

    def set_source_volume(self, node_name, volume):
        self.source_volume_calls.append((node_name, volume))


class _FakeWindow:
    def __init__(self):
        self.runtime = _FakeRuntime()
        self.submix_state = {}


class ChannelStripMicGainTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls._app = QApplication.instance() or QApplication([])

    def test_mic_strip_exposes_hardware_gain_control_and_commits_through_runtime(self):
        strip = ChannelStrip("55", "mic", "Mic", "Microphone", "mic", engine=None)
        strip._main_win = _FakeWindow()
        self.addCleanup(strip.deleteLater)

        self.assertIsNotNone(strip.src_slider)
        self.assertEqual(strip.src_vol_lbl.text(), "100%")

        strip._on_src_vol(68)
        strip._commit_src_vol()

        self.assertEqual(strip._main_win.runtime.source_volume_calls, [("mic", 0.68)])

        strip.update_from_node(1.0, False, 1.0, False, False, source_vol=0.55, source_mute=False)
        self.assertEqual(strip.src_slider.value(), 55)
        self.assertEqual(strip.src_vol_lbl.text(), "55%")

    def test_virtual_strip_does_not_render_hardware_gain_control(self):
        strip = ChannelStrip("77", "wavelinux_music", "Music", "Virtual", "music", engine=None)
        self.addCleanup(strip.deleteLater)

        self.assertIsNone(strip.src_slider)
        self.assertIsNone(strip.src_vol_lbl)

    def test_apply_scale_compacts_strip_height_when_slider_height_drops(self):
        strip = ChannelStrip("77", "wavelinux_music", "Music", "Virtual", "music", engine=None)
        self.addCleanup(strip.deleteLater)
        strip.show()
        self._app.processEvents()

        tall_metrics = MixerStripMetrics(
            strip_width=180,
            slider_height=140,
            strip_height=0,
            outer_margin=6,
            inner_spacing=4,
            fader_spacing=10,
            peak_height=5,
            link_button_size=24,
            mute_button_size=28,
            mic_gain_height=20,
            use_horizontal_scroll=False,
        )
        short_metrics = MixerStripMetrics(
            strip_width=180,
            slider_height=90,
            strip_height=0,
            outer_margin=4,
            inner_spacing=2,
            fader_spacing=7,
            peak_height=4,
            link_button_size=20,
            mute_button_size=24,
            mic_gain_height=16,
            use_horizontal_scroll=False,
        )
        strip.apply_scale(tall_metrics)
        self._app.processEvents()
        tall_height = strip.height()
        tall_slider = strip.mon_slider.height()

        strip.apply_scale(short_metrics)
        self._app.processEvents()

        self.assertLess(strip.height(), tall_height)
        self.assertLess(strip.mon_slider.height(), tall_slider)
        self.assertEqual(strip.mon_slider.height(), 98)
        self.assertEqual(strip.str_slider.height(), 98)

    def test_mic_and_virtual_strips_share_the_same_scaled_card_height(self):
        mic = ChannelStrip("55", "mic", "Mic", "Microphone", "mic", engine=None)
        virt = ChannelStrip("77", "wavelinux_music", "Music", "Virtual", "music", engine=None)
        self.addCleanup(mic.deleteLater)
        self.addCleanup(virt.deleteLater)
        mic.show()
        virt.show()
        self._app.processEvents()

        metrics = MixerStripMetrics(
            strip_width=158,
            slider_height=80,
            strip_height=0,
            outer_margin=3,
            inner_spacing=2,
            fader_spacing=6,
            peak_height=4,
            link_button_size=20,
            mute_button_size=24,
            mic_gain_height=16,
            use_horizontal_scroll=False,
        )
        target = max(mic.measure_scaled_height(metrics), virt.measure_scaled_height(metrics))
        mic.apply_scale(metrics, target_height=target)
        virt.apply_scale(metrics, target_height=target)
        self._app.processEvents()

        self.assertEqual(mic.height(), virt.height())


if __name__ == "__main__":
    unittest.main()
