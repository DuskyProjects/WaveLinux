"""Synthetic playback and monitor capture helpers."""

from __future__ import annotations

import math
import os
import signal
import struct
import subprocess
import tempfile
import time
import wave


def generate_probe_wav(*, duration_s=2.0, sample_rate=48000, amplitude=0.03, freq_hz=440.0, path=None):
    if path is None:
        fd, path = tempfile.mkstemp(prefix="wavelinux-probe-", suffix=".wav")
        os.close(fd)
    total_frames = int(sample_rate * duration_s)
    with wave.open(path, "wb") as handle:
        handle.setnchannels(2)
        handle.setsampwidth(2)
        handle.setframerate(sample_rate)
        frames = []
        for frame_index in range(total_frames):
            sample = int(
                max(-1.0, min(1.0, amplitude * math.sin(2.0 * math.pi * freq_hz * frame_index / sample_rate)))
                * 32767
            )
            frames.append(struct.pack("<hh", sample, sample))
        handle.writeframes(b"".join(frames))
    return path


def generate_speech_like_probe_wav(*, duration_s=2.0, sample_rate=48000, amplitude=0.10, path=None):
    """Generate deterministic vowel-like test audio that RNNoise should treat as speech."""
    if path is None:
        fd, path = tempfile.mkstemp(prefix="wavelinux-voice-probe-", suffix=".wav")
        os.close(fd)

    formant_sets = (
        (720.0, 1220.0, 2600.0),
        (300.0, 2150.0, 3000.0),
        (520.0, 950.0, 2450.0),
        (390.0, 1900.0, 2550.0),
    )
    total_frames = int(sample_rate * duration_s)
    # Synthesize one repeating phrase instead of doing expensive formant math
    # across long stress runs. RNNoise still sees speech-like content, but
    # runner startup stays fast enough for tight diagnostics.
    phrase_frames = min(total_frames, int(sample_rate * 3.36))
    phase = 0.0
    noise_state = 0x5EED1234
    frames = []

    for frame_index in range(phrase_frames):
        t = frame_index / float(sample_rate)
        syllable_pos = t % 0.42
        syllable_index = int(t / 0.42) % len(formant_sets)
        formants = formant_sets[syllable_index]
        voiced = 0.045 <= syllable_pos <= 0.34
        if voiced:
            attack = min(1.0, max(0.0, (syllable_pos - 0.045) / 0.045))
            release = min(1.0, max(0.0, (0.34 - syllable_pos) / 0.07))
            envelope = min(attack, release)
        else:
            envelope = 0.0

        f0 = 118.0 + 18.0 * math.sin(2.0 * math.pi * 2.1 * t) + 5.0 * math.sin(2.0 * math.pi * 5.3 * t)
        phase += 2.0 * math.pi * f0 / sample_rate
        voiced_sample = 0.0
        for harmonic in range(1, 28):
            freq = harmonic * f0
            formant_gain = 0.035
            formant_gain += 1.20 * math.exp(-0.5 * ((freq - formants[0]) / 110.0) ** 2)
            formant_gain += 0.75 * math.exp(-0.5 * ((freq - formants[1]) / 170.0) ** 2)
            formant_gain += 0.35 * math.exp(-0.5 * ((freq - formants[2]) / 260.0) ** 2)
            voiced_sample += (formant_gain / harmonic) * math.sin(phase * harmonic)

        noise_state = (1664525 * noise_state + 1013904223) & 0xFFFFFFFF
        breath = ((noise_state / 0xFFFFFFFF) * 2.0 - 1.0)
        consonant = 0.0
        if syllable_pos < 0.035:
            consonant = breath * (1.0 - syllable_pos / 0.035)

        sample = amplitude * ((0.82 * envelope * voiced_sample) + (0.10 * consonant))
        sample = max(-1.0, min(1.0, sample))
        pcm = int(sample * 32767)
        frames.append(struct.pack("<hh", pcm, pcm))

    phrase = b"".join(frames)
    if total_frames > phrase_frames and phrase_frames > 0:
        repeats, remainder = divmod(total_frames, phrase_frames)
        payload = phrase * repeats + phrase[:remainder * 4]
    else:
        payload = phrase

    with wave.open(path, "wb") as handle:
        handle.setnchannels(2)
        handle.setsampwidth(2)
        handle.setframerate(sample_rate)
        handle.writeframes(payload)
    return path


def spawn_probe_stream(wav_path, *, sink_name=None, app_name, stream_name, media_role="music", volume=65536):
    cmd = [
        "paplay",
        f"--client-name={app_name}",
        f"--stream-name={stream_name}",
        f"--volume={int(volume)}",
        "--property=application.name=" + app_name,
        "--property=application.process.binary=" + app_name.lower(),
        "--property=media.role=" + media_role,
    ]
    if sink_name:
        cmd.append(f"--device={sink_name}")
    cmd.append(wav_path)
    return subprocess.Popen(
        cmd,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        preexec_fn=os.setsid,
    )


def stop_probe_stream(proc):
    if proc is None:
        return
    if proc.poll() is not None:
        return
    try:
        os.killpg(proc.pid, signal.SIGTERM)
    except OSError:
        return
    try:
        proc.wait(timeout=2.0)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except OSError:
            pass


def _spawn_capture_process(source_name, *, sample_rate=48000):
    cmd = [
        "parec",
        f"--device={source_name}",
        "--raw",
        f"--rate={sample_rate}",
        "--channels=2",
        "--format=s16le",
    ]
    return subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )


def _collect_capture_process(proc, *, source_name):
    if proc.poll() is None:
        proc.terminate()
    try:
        stdout, stderr = proc.communicate(timeout=2.0)
    except subprocess.TimeoutExpired:
        proc.kill()
        stdout, stderr = proc.communicate(timeout=2.0)
    count = len(stdout) // 2
    values = struct.unpack("<" + "h" * count, stdout) if count else ()
    peak = max((abs(value) for value in values), default=0)
    rms = ((sum(value * value for value in values) / len(values)) ** 0.5) if values else 0.0
    return {
        "source_name": source_name,
        "bytes": len(stdout),
        "peak": peak,
        "rms": rms,
        "stderr": (stderr or b"").decode("utf-8", errors="replace").strip(),
    }


def _pcm_capture_stats(source_name, data, *, stderr=""):
    count = len(data) // 2
    values = struct.unpack("<" + "h" * count, data) if count else ()
    peak = max((abs(value) for value in values), default=0)
    rms = ((sum(value * value for value in values) / len(values)) ** 0.5) if values else 0.0
    return {
        "source_name": source_name,
        "bytes": len(data),
        "peak": peak,
        "rms": rms,
        "stderr": str(stderr or "").strip(),
    }


def _capture_source_audio_pw_record(source_name, *, duration_s=1.0, sample_rate=48000):
    fd, path = tempfile.mkstemp(prefix="wavelinux-source-capture-", suffix=".raw")
    os.close(fd)
    proc = None
    try:
        proc = subprocess.Popen(
            [
                "pw-record",
                "--target",
                source_name,
                "--rate",
                str(int(sample_rate)),
                "--channels",
                "2",
                "--format",
                "s16",
                path,
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
        )
        time.sleep(max(0.0, float(duration_s)))
        if proc.poll() is None:
            proc.terminate()
        try:
            _, stderr_text = proc.communicate(timeout=2.0)
        except subprocess.TimeoutExpired:
            proc.kill()
            _, stderr_text = proc.communicate(timeout=2.0)
        with open(path, "rb") as handle:
            data = handle.read()
        return _pcm_capture_stats(source_name, data, stderr=stderr_text)
    finally:
        try:
            os.unlink(path)
        except OSError:
            pass


def capture_source_audio(source_name, *, duration_s=1.0, sample_rate=48000):
    try:
        return _capture_source_audio_pw_record(
            source_name,
            duration_s=duration_s,
            sample_rate=sample_rate,
        )
    except FileNotFoundError:
        proc = _spawn_capture_process(source_name, sample_rate=sample_rate)
        time.sleep(max(0.0, float(duration_s)))
        return _collect_capture_process(proc, source_name=source_name)


def probe_route(*, wav_path, sink_name, capture_sources, duration_s=1.0, app_name="StressBrowser", stream_name="Stress Browser", media_role="music"):
    capture_procs = {
        source_name: _spawn_capture_process(source_name)
        for source_name in (capture_sources or [])
    }
    try:
        time.sleep(0.25)
        proc = spawn_probe_stream(
            wav_path,
            sink_name=sink_name,
            app_name=app_name,
            stream_name=stream_name,
            media_role=media_role,
        )
        time.sleep(max(0.0, float(duration_s)))
    finally:
        stop_probe_stream(locals().get("proc"))
    return {
        source_name: _collect_capture_process(proc, source_name=source_name)
        for source_name, proc in capture_procs.items()
    }
