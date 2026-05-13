import struct
import unittest

from main import MeterWorker


class MeterWorkerTests(unittest.TestCase):
    def test_frame_peak_reports_normalized_peak(self):
        frame = struct.pack("<4h", 0, 16384, -8192, 4096)

        peak = MeterWorker._frame_peak(frame, 0.0)

        self.assertAlmostEqual(peak, 0.5, places=3)

    def test_frame_peak_applies_release_envelope(self):
        loud_frame = struct.pack("<4h", 32767, 0, 0, 0)
        quiet_frame = struct.pack("<4h", 0, 0, 0, 0)

        first = MeterWorker._frame_peak(loud_frame, 0.0)
        second = MeterWorker._frame_peak(quiet_frame, first)

        self.assertGreater(first, 0.99)
        self.assertAlmostEqual(second, first * 0.6, places=3)


if __name__ == "__main__":
    unittest.main()
