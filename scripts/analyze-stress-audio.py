#!/usr/bin/env python3
"""Analyze a deterministic WaveLinux stress capture for silence and discontinuities.

The live stress fixture is antiphase stereo. Analyzing (left - right) / 2
rejects centered microphone and application audio while preserving the pilot.
Block correlation then detects lost audio and persistent sample slips without
retaining one Python object per captured frame during hour-long runs.
"""

from __future__ import annotations

import argparse
import array
import json
import math
import sys
from pathlib import Path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("input", type=Path)
    parser.add_argument("--rate", type=int, default=48_000)
    parser.add_argument("--channels", type=int, default=2)
    parser.add_argument("--frequency", type=float, default=4_000.0)
    parser.add_argument("--amplitude", type=int, default=12_000)
    parser.add_argument(
        "--channel-mode",
        choices=("first", "antiphase"),
        default="first",
    )
    parser.add_argument("--expected-duration", type=float, required=True)
    parser.add_argument("--duration-tolerance", type=float, default=2.0)
    parser.add_argument("--silence-threshold", type=int, default=800)
    parser.add_argument("--silence-msec", type=float, default=50.0)
    parser.add_argument("--block-msec", type=float, default=10.0)
    parser.add_argument("--minimum-tone-ratio", type=float, default=0.35)
    parser.add_argument("--phase-jump-radians", type=float, default=0.25)
    parser.add_argument("--edge-ignore-msec", type=float, default=250.0)
    return parser.parse_args()


def percentile(values: list[float], fraction: float) -> float | None:
    if not values:
        return None
    values.sort()
    index = min(len(values) - 1, max(0, math.ceil(len(values) * fraction) - 1))
    return values[index]


def circular_distance(left: float, right: float) -> float:
    return abs((left - right + math.pi) % math.tau - math.pi)


def main() -> int:
    args = parse_args()
    if args.rate <= 0 or args.channels <= 0 or args.block_msec <= 0:
        raise SystemExit("rate, channels, and block-msec must be positive")
    if not 0 < args.frequency < args.rate / 2:
        raise SystemExit("frequency must be between 0 Hz and Nyquist")
    if not 0 < args.minimum_tone_ratio <= 1:
        raise SystemExit("minimum-tone-ratio must be in (0, 1]")
    if not args.input.is_file():
        raise SystemExit(f"capture does not exist: {args.input}")

    frame_bytes = args.channels * 2
    file_size = args.input.stat().st_size
    total_frames = file_size // frame_bytes
    duration_sec = total_frames / args.rate
    trailing_bytes = file_size % frame_bytes
    ignore_frames = round(args.edge_ignore_msec * args.rate / 1_000)
    silence_limit_frames = max(1, round(args.silence_msec * args.rate / 1_000))
    block_frames = max(1, round(args.block_msec * args.rate / 1_000))
    phase_step = math.tau * args.frequency / args.rate
    cosine = [math.cos(phase_step * index) for index in range(block_frames)]
    sine = [math.sin(phase_step * index) for index in range(block_frames)]

    peak = 0
    sum_squares = 0.0
    analyzed_samples = 0
    longest_silence_frames = 0
    current_silence_frames = 0
    current_silence_start_frame: int | None = None
    silence_intervals = 0
    silence_events: list[dict[str, float]] = []
    tone_amplitudes: list[float] = []
    phase_jumps: list[float] = []
    low_tone_blocks = 0
    low_tone_events = 0
    low_tone_event_frames: list[int] = []
    phase_discontinuity_blocks = 0
    phase_discontinuity_events = 0
    phase_discontinuity_events_detail: list[dict[str, float]] = []
    in_low_tone = False
    in_phase_discontinuity = False
    previous_phase: float | None = None
    block_real = 0.0
    block_imag = 0.0
    block_count = 0
    block_start_frame = 0
    clipped_samples = 0
    frame_index = 0

    with args.input.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            samples = array.array("h")
            samples.frombytes(chunk[: len(chunk) - (len(chunk) % 2)])
            if sys.byteorder != "little":
                samples.byteswap()
            usable = len(samples) - (len(samples) % args.channels)
            for offset in range(0, usable, args.channels):
                left = samples[offset]
                right = samples[offset + 1] if args.channels >= 2 else left
                if args.channel_mode == "antiphase" and args.channels >= 2:
                    value = (float(left) - float(right)) * 0.5
                else:
                    value = float(left)
                frame_peak = max(
                    abs(samples[offset + channel]) for channel in range(args.channels)
                )
                peak = max(peak, frame_peak)
                clipped_samples += sum(
                    abs(samples[offset + channel]) >= 32_767
                    for channel in range(args.channels)
                )
                sum_squares += value * value
                analyzed_samples += 1

                inside_edges = ignore_frames <= frame_index < total_frames - ignore_frames
                if inside_edges and frame_peak < args.silence_threshold:
                    if current_silence_frames == 0:
                        current_silence_start_frame = frame_index
                    current_silence_frames += 1
                    longest_silence_frames = max(
                        longest_silence_frames, current_silence_frames
                    )
                else:
                    if current_silence_frames >= silence_limit_frames:
                        silence_intervals += 1
                        silence_events.append(
                            {
                                "start_sec": (
                                    current_silence_start_frame or 0
                                )
                                / args.rate,
                                "duration_msec": current_silence_frames
                                * 1_000
                                / args.rate,
                            }
                        )
                    current_silence_frames = 0
                    current_silence_start_frame = None

                block_real += value * cosine[block_count]
                block_imag -= value * sine[block_count]
                block_count += 1

                frame_index += 1
                if block_count == block_frames or frame_index == total_frames:
                    block_end_frame = frame_index
                    block_inside_edges = (
                        block_start_frame >= ignore_frames
                        and block_end_frame <= total_frames - ignore_frames
                    )
                    if block_inside_edges:
                        tone_amplitude = (
                            2.0 * math.hypot(block_real, block_imag) / block_count
                        )
                        absolute_phase = math.atan2(block_imag, block_real)
                        expected_phase = (phase_step * block_start_frame) % math.tau
                        tone_phase = (
                            absolute_phase - expected_phase + math.pi
                        ) % math.tau - math.pi
                        tone_amplitudes.append(tone_amplitude)
                        tone_is_low = (
                            tone_amplitude
                            < args.amplitude * args.minimum_tone_ratio
                        )
                        if tone_is_low:
                            low_tone_blocks += 1
                            if not in_low_tone:
                                low_tone_events += 1
                                low_tone_event_frames.append(block_start_frame)
                        in_low_tone = tone_is_low

                        if previous_phase is not None and not tone_is_low:
                            phase_jump = circular_distance(tone_phase, previous_phase)
                            phase_jumps.append(phase_jump)
                            phase_is_discontinuous = phase_jump > args.phase_jump_radians
                            if phase_is_discontinuous:
                                phase_discontinuity_blocks += 1
                                if not in_phase_discontinuity:
                                    phase_discontinuity_events += 1
                                    phase_discontinuity_events_detail.append(
                                        {
                                            "start_sec": block_start_frame / args.rate,
                                            "jump_radians": phase_jump,
                                        }
                                    )
                            in_phase_discontinuity = phase_is_discontinuous
                        else:
                            in_phase_discontinuity = False
                        if not tone_is_low:
                            previous_phase = tone_phase

                    block_real = 0.0
                    block_imag = 0.0
                    block_count = 0
                    block_start_frame = frame_index

    if current_silence_frames >= silence_limit_frames:
        silence_intervals += 1
        silence_events.append(
            {
                "start_sec": (current_silence_start_frame or 0) / args.rate,
                "duration_msec": current_silence_frames * 1_000 / args.rate,
            }
        )

    duration_error_sec = abs(duration_sec - args.expected_duration)
    rms = math.sqrt(sum_squares / analyzed_samples) if analyzed_samples else 0.0
    tone_amplitude_p01 = percentile(tone_amplitudes, 0.01)
    tone_amplitude_p50 = percentile(tone_amplitudes, 0.50)
    phase_jump_p99 = percentile(phase_jumps, 0.99)
    phase_jump_max = max(phase_jumps, default=None)
    continuity_pass = (
        trailing_bytes == 0
        and duration_error_sec <= args.duration_tolerance
        and longest_silence_frames < silence_limit_frames
        and low_tone_events == 0
        and phase_discontinuity_events == 0
    )
    report = {
        "input": str(args.input),
        "sample_rate_hz": args.rate,
        "channels": args.channels,
        "tone_frequency_hz": args.frequency,
        "expected_duration_sec": args.expected_duration,
        "duration_sec": duration_sec,
        "duration_error_sec": duration_error_sec,
        "frames": total_frames,
        "trailing_bytes": trailing_bytes,
        "peak_s16": peak,
        "clipped_samples": clipped_samples,
        "rms_s16": rms,
        "longest_silence_msec": longest_silence_frames * 1_000 / args.rate,
        "silence_intervals": silence_intervals,
        "silence_events": silence_events,
        "analysis_block_msec": args.block_msec,
        "tone_amplitude_p01": tone_amplitude_p01,
        "tone_amplitude_p50": tone_amplitude_p50,
        "low_tone_blocks": low_tone_blocks,
        "low_tone_events": low_tone_events,
        "low_tone_event_offsets_sec": [
            frame / args.rate for frame in low_tone_event_frames
        ],
        "phase_jump_p99_radians": phase_jump_p99,
        "phase_jump_max_radians": phase_jump_max,
        "phase_discontinuity_blocks": phase_discontinuity_blocks,
        "phase_discontinuity_events": phase_discontinuity_events,
        "phase_discontinuity_events_detail": phase_discontinuity_events_detail,
        "continuity_pass": continuity_pass,
    }
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0 if continuity_pass else 1


if __name__ == "__main__":
    raise SystemExit(main())
