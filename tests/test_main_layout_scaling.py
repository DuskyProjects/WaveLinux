import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import QApplication, QWidget

from main import ChannelStrip, WaveLinuxWindow


class _FakeMargins:
    def __init__(self, left=4, right=4):
        self._left = left
        self._right = right

    def left(self):
        return self._left

    def right(self):
        return self._right


class _FakeLayout:
    def __init__(self, spacing=10, left=4, right=4):
        self._spacing = spacing
        self._margins = _FakeMargins(left=left, right=right)

    def spacing(self):
        return self._spacing

    def contentsMargins(self):
        return self._margins


class _FakeViewport:
    def __init__(self, width, height):
        self._width = width
        self._height = height

    def width(self):
        return self._width

    def height(self):
        return self._height

    def resize(self, width, height):
        self._width = width
        self._height = height


class _FakeScroll:
    def __init__(self, width, height):
        self._viewport = _FakeViewport(width, height)
        self._policy = Qt.ScrollBarPolicy.ScrollBarAsNeeded

    def viewport(self):
        return self._viewport

    def setHorizontalScrollBarPolicy(self, policy):
        self._policy = policy

    def horizontalScrollBarPolicy(self):
        return self._policy


class MixerLayoutScalingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls._app = QApplication.instance() or QApplication([])

    def _window(self, width, height, *, strip_count=6, include_mic=True):
        win = WaveLinuxWindow.__new__(WaveLinuxWindow)
        win.inputs_scroll = _FakeScroll(width, height)
        win.input_layout = _FakeLayout()
        win.inputs_container = QWidget()
        win.channel_widgets = {}
        for index in range(strip_count):
            is_mic = include_mic and index == 0
            strip = ChannelStrip(
                str(index),
                "alsa_input.internal" if is_mic else f"wavelinux_{index}",
                "Digital Microphone" if is_mic else f"Channel {index}",
                "Microphone" if is_mic else "Virtual",
                "🎤" if is_mic else "🎵",
                engine=None,
            )
            strip.show()
            self.addCleanup(strip.deleteLater)
            win.channel_widgets[str(index)] = strip
        self._app.processEvents()
        return win

    def _used_row_width(self, win):
        strips = list(win.channel_widgets.values())
        margins = win.input_layout.contentsMargins()
        spacing = win.input_layout.spacing()
        return (
            sum(strip.width() for strip in strips)
            + spacing * max(0, len(strips) - 1)
            + margins.left()
            + margins.right()
        )

    def test_compute_metrics_grow_strip_width_on_wide_viewport(self):
        win = self._window(1904, 620, strip_count=6)

        metrics = win._compute_mixer_strip_metrics()

        self.assertGreater(metrics.strip_width, 280)
        self.assertFalse(metrics.use_horizontal_scroll)

    def test_rescale_consumes_most_available_width_without_scroll(self):
        win = self._window(1904, 620, strip_count=6)

        win._rescale_strips()
        self._app.processEvents()

        self.assertEqual(
            win.inputs_scroll.horizontalScrollBarPolicy(),
            Qt.ScrollBarPolicy.ScrollBarAlwaysOff,
        )
        self.assertGreaterEqual(self._used_row_width(win), int(1904 * 0.92))

    def test_rescale_enables_horizontal_scroll_only_at_minimum_width(self):
        win = self._window(800, 620, strip_count=8)

        win._rescale_strips()
        self._app.processEvents()

        self.assertEqual(
            win.inputs_scroll.horizontalScrollBarPolicy(),
            Qt.ScrollBarPolicy.ScrollBarAsNeeded,
        )
        self.assertTrue(
            all(strip.width() == win._MIN_STRIP_W for strip in win.channel_widgets.values())
        )

    def test_rescale_compacts_strip_and_slider_height_when_viewport_shortens(self):
        win = self._window(1400, 620, strip_count=6)

        win._rescale_strips()
        self._app.processEvents()
        tall_strip_height = next(iter(win.channel_widgets.values())).height()
        tall_slider_height = next(iter(win.channel_widgets.values())).mon_slider.height()

        win.inputs_scroll.viewport().resize(1400, 300)
        win._rescale_strips()
        self._app.processEvents()

        short_strip = next(iter(win.channel_widgets.values()))
        self.assertLess(short_strip.height(), tall_strip_height)
        self.assertLess(short_strip.mon_slider.height(), tall_slider_height)

    def test_rescale_is_idempotent_for_same_viewport(self):
        win = self._window(1280, 420, strip_count=5)

        win._rescale_strips()
        self._app.processEvents()
        first = [
            (strip.width(), strip.height(), strip.mon_slider.height(), strip.str_slider.height())
            for strip in win.channel_widgets.values()
        ]

        win._rescale_strips()
        self._app.processEvents()
        second = [
            (strip.width(), strip.height(), strip.mon_slider.height(), strip.str_slider.height())
            for strip in win.channel_widgets.values()
        ]

        self.assertEqual(first, second)


if __name__ == "__main__":
    unittest.main()
