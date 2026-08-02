#!/usr/bin/env python3
"""Generate a continuous deterministic stereo s16le tone for audio stress tests."""

from __future__ import annotations

import argparse
import math
import struct
import sys


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rate", type=int, default=48_000)
    parser.add_argument("--frequency", type=float, default=4_000.0)
    parser.add_argument("--amplitude", type=int, default=12_000)
    parser.add_argument("--channels", type=int, default=2)
    parser.add_argument(
        "--channel-mode",
        choices=("identical", "antiphase"),
        default="identical",
        help="emit the pilot equally or with opposite stereo polarity",
    )
    parser.add_argument("--chunk-frames", type=int, default=4_096)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.rate <= 0 or args.channels <= 0 or args.chunk_frames <= 0:
        raise SystemExit("rate, channels, and chunk-frames must be positive")
    if not 0 < args.frequency < args.rate / 2:
        raise SystemExit("frequency must be between 0 Hz and Nyquist")
    if not 0 < args.amplitude <= 32_767:
        raise SystemExit("amplitude must be in the s16 range")

    phase = 0.0
    phase_step = math.tau * args.frequency / args.rate
    output = sys.stdout.buffer
    try:
        while True:
            samples: list[int] = []
            for _ in range(args.chunk_frames):
                value = round(args.amplitude * math.sin(phase))
                if args.channel_mode == "antiphase" and args.channels >= 2:
                    samples.extend((value, -value))
                    samples.extend([value] * (args.channels - 2))
                else:
                    samples.extend([value] * args.channels)
                phase += phase_step
                if phase >= math.tau:
                    phase -= math.tau
            output.write(struct.pack(f"<{len(samples)}h", *samples))
            output.flush()
    except (BrokenPipeError, KeyboardInterrupt):
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
