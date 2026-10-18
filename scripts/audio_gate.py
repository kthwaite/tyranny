#!/usr/bin/env python3
"""Numerical audio-path gate; Python standard library plus ffmpeg and audio-probe.

Run after building the probe:
    python3 scripts/audio_gate.py --probe target/debug/audio-probe
    python3 scripts/audio_gate.py --probe target/debug/audio-probe --coreaudio

Every invocation creates a fresh run directory beneath --work-dir. No existing
fixtures or user media are changed. report.json contains commands, raw process
output, thresholds, and separate numerical/device/manual-evidence conclusions.
An unavailable encoder, decoder, or GStreamer plugin is a failure, never a pass.

Lossless output must match PCM24 source samples and length. Lossy output must
retain the exact source length and satisfy explicitly bounded, unshifted error
checks against both source and separately ffmpeg-decoded tracks. Diagnostic lag
searches NEVER change samples, alignment, length checks, or pass/fail decisions.
Different resamplers are allowed small errors in mixed-rate tests; these tests
are deliberately NOT same-rate or bit-perfect gapless evidence.

Exit 0 means all numerical checks, queue mutation checks, and any requested
device checks passed. It does not certify human listening, packaged-app
deployment, or the entire SPIKE.md gate. Those limitations remain in the report.
"""

from __future__ import annotations

import argparse
from array import array
from datetime import datetime, timezone
import json
import math
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import wave


RATE = 48000
CHANNELS = 2
PART_FRAMES = (48137, 52979, 33611)
LOSSLESS_EPSILON = 1.0e-7
LOSSY_SOURCE_RMS = 0.012
LOSSY_SOURCE_MAX = 0.08
LOSSY_REFERENCE_RMS = 0.004
LOSSY_REFERENCE_MAX = 0.05
LOSSY_SEAM_ERROR = 0.02
GAIN_DB_TOLERANCE = 0.12
FORMATS = {
    "wav": ("wav", ["-c:a", "pcm_s24le"], True),
    "flac": ("flac", ["-c:a", "flac"], True),
    "mp3": ("mp3", ["-c:a", "libmp3lame", "-b:a", "192k"], False),
    "mp3_lame": ("mp3", [], False),
    "vorbis": ("ogg", [], False),
    "opus": ("opus", ["-c:a", "libopus", "-b:a", "160k"], False),
    "aac_mp4": ("m4a", ["-c:a", "aac", "-b:a", "192k"], False),
}


class CaseFailure(Exception):
    pass


def signal_sample(frame: int, channel: int, rate: int) -> float:
    """Continuous, non-periodically-repeating test signal, below 0.12 full scale."""
    t = frame / rate
    if channel == 0:
        return (0.065 * math.sin(2 * math.pi * (233 * t + 7 * t * t) + 0.17)
                + 0.023 * math.sin(2 * math.pi * 997 * t + 0.51))
    return (0.059 * math.sin(2 * math.pi * (317 * t + 11 * t * t) + 0.73)
            + 0.021 * math.sin(2 * math.pi * 1231 * t + 0.31))


def make_wav(path: Path, frames: int, start: int = 0,
             rate: int = RATE, channels: int = CHANNELS) -> array:
    """Return the exact quantized source, not the pre-quantization waveform."""
    pcm = bytearray()
    samples = array("f")
    for frame in range(start, start + frames):
        for channel in range(channels):
            value = round(signal_sample(frame, channel, rate) * 8388608)
            pcm.extend((value & 0xFFFFFF).to_bytes(3, "little"))
            samples.append(value / 8388608)
    with wave.open(str(path), "wb") as handle:
        handle.setnchannels(channels)
        handle.setsampwidth(3)
        handle.setframerate(rate)
        handle.writeframes(pcm)
    return samples


def load_pcm(path: Path) -> array:
    if not path.is_file():
        raise CaseFailure(f"PCM capture missing: {path}")
    data = path.read_bytes()
    if len(data) % 8:
        raise CaseFailure(f"PCM byte count {len(data)} is not interleaved stereo f32")
    values = array("f")
    values.frombytes(data)
    if sys.byteorder != "little":
        values.byteswap()
    if not all(math.isfinite(value) for value in values):
        raise CaseFailure("PCM contains NaN or infinity")
    return values


def rms(values) -> float:
    return math.sqrt(math.fsum(value * value for value in values) / len(values)) if values else 0.0


def comparison(actual: array, expected: array) -> dict:
    common = min(len(actual), len(expected))
    squared = 0.0
    maximum = 0.0
    for index in range(common):
        error = actual[index] - expected[index]
        squared += error * error
        maximum = max(maximum, abs(error))
    return {
        "actual_frames": len(actual) // CHANNELS,
        "expected_frames": len(expected) // CHANNELS,
        "frame_delta": (len(actual) - len(expected)) // CHANNELS,
        "compared_frames": common // CHANNELS,
        "rms_error": math.sqrt(squared / common) if common else None,
        "max_abs_error": maximum if common else None,
        "alignment": "unshifted; common prefix only; length checked separately",
    }


def comparison_ok(result: dict, rms_limit: float, max_limit: float,
                  frame_tolerance: int = 0) -> bool:
    return (result["compared_frames"] > 0
            and abs(result["frame_delta"]) <= frame_tolerance
            and result["rms_error"] <= rms_limit
            and result["max_abs_error"] <= max_limit)


def boundaries(actual: array, expected: array, positions: list[int]) -> list[dict]:
    result = []
    for frame in positions:
        item = {"expected_frame": frame, "window_radius_frames": 128}
        if frame < 1 or (frame + 1) * 2 > min(len(actual), len(expected)):
            item.update(available=False, passed=False)
        else:
            start = max(0, frame - 128) * 2
            stop = min(len(actual), len(expected), (frame + 128) * 2)
            actual_delta = [actual[frame * 2 + c] - actual[(frame - 1) * 2 + c]
                            for c in range(2)]
            expected_delta = [expected[frame * 2 + c] - expected[(frame - 1) * 2 + c]
                              for c in range(2)]
            longest = current = 0
            for offset in range(start, stop, 2):
                current = current + 1 if max(abs(actual[offset]), abs(actual[offset + 1])) < 1e-8 else 0
                longest = max(longest, current)
            item.update(
                available=True,
                actual_adjacent_delta=actual_delta,
                reference_adjacent_delta=expected_delta,
                delta_error=max(abs(a - b) for a, b in zip(actual_delta, expected_delta)),
                window=comparison(actual[start:stop], expected[start:stop]),
                longest_near_zero_run_frames=longest,
            )
        result.append(item)
    return result


def diagnostic_lag(actual: array, expected: array, start_frame: int) -> dict:
    """Sparse bounded search, diagnostic only: positive lag = capture is delayed."""
    offsets = range(0, 4096, 23)
    expected_frames = len(expected) // 2
    actual_frames = len(actual) // 2
    probes = [start_frame + offset for offset in offsets
              if start_frame + offset < expected_frames]
    best = None
    for lag in range(-2048, 2049):
        valid = [frame for frame in probes if 0 <= frame + lag < actual_frames]
        if len(valid) < 64:
            continue
        error = math.fsum((actual[(frame + lag) * 2] - expected[frame * 2]) ** 2
                          for frame in valid) / len(valid)
        if best is None or error < best[0]:
            best = (error, lag, len(valid))
    return {
        "source_start_frame": start_frame,
        "searched_lag_frames": [-2048, 2048],
        "best_lag_frames": best[1] if best else None,
        "sparse_rms_error": math.sqrt(best[0]) if best else None,
        "used_for_acceptance": False,
        "limitation": "bounded sparse correlation, not a proof of encoder delay or padding",
    }


class Gate:
    def __init__(self, args):
        self.probe = str(Path(args.probe).expanduser().resolve())
        self.clocked = args.clocked
        root = Path(args.work_dir).expanduser().resolve()
        root.mkdir(parents=True, exist_ok=True)
        self.directory = Path(tempfile.mkdtemp(prefix="run-", dir=root))
        self.ffmpeg = shutil.which("ffmpeg") or "ffmpeg"
        self.report = {
            "schema_version": 1,
            "started_utc": datetime.now(timezone.utc).isoformat(),
            "run_directory": str(self.directory),
            "invocation": sys.argv,
            "python": sys.version,
            "platform": sys.platform,
            "probe": self.probe,
            "ffmpeg": self.ffmpeg,
            "output_contract": {"rate": RATE, "channels": 2, "format": "interleaved F32LE"},
            "capture_clocked": self.clocked,
            "thresholds": {
                "lossless_max_abs_error": LOSSLESS_EPSILON,
                "lossy_source_rms_error": LOSSY_SOURCE_RMS,
                "lossy_source_max_abs_error": LOSSY_SOURCE_MAX,
                "lossy_ffmpeg_rms_error": LOSSY_REFERENCE_RMS,
                "lossy_ffmpeg_max_abs_error": LOSSY_REFERENCE_MAX,
                "lossy_boundary_delta_error": LOSSY_SEAM_ERROR,
                "gain_db_tolerance": GAIN_DB_TOLERANCE,
                "same_rate_frame_tolerance": 0,
                "mixed_rate_frame_tolerance_per_track": 2,
            },
            "commands": [],
            "cases": [],
            "limitations": [
                "PCM capture does not establish device latency, underflow freedom, or audible quality.",
                "CoreAudio command success is device-path evidence, not human listening confirmation.",
                "Sparse lag diagnostics never trim, shift, or normalize captured samples.",
                "Lossy ffmpeg references may retain encoder padding; source lengths are checked independently.",
                "Mixed-rate/channel results are separate from same-rate gapless results.",
                "Transport summary confirms the probe's exercise, not subjective timing or seek accuracy.",
                "Queue mutations are exercised through the real playback owner and captured PCM.",
            ],
        }
        self.format_paths = {}

    def command(self, argv: list[str], timeout: int = 75) -> dict:
        start = time.monotonic()
        record = {"argv": argv, "timeout_seconds": timeout}
        try:
            process = subprocess.run(argv, capture_output=True, text=True,
                                     errors="replace", timeout=timeout, check=False)
            record.update(returncode=process.returncode, stdout=process.stdout,
                          stderr=process.stderr, timed_out=False)
        except subprocess.TimeoutExpired as error:
            def text(value):
                return value.decode("utf-8", "replace") if isinstance(value, bytes) else (value or "")
            record.update(returncode=None, stdout=text(error.stdout), stderr=text(error.stderr), timed_out=True)
        except OSError as error:
            record.update(returncode=None, stdout="", stderr=str(error), timed_out=False)
        record["elapsed_seconds"] = time.monotonic() - start
        self.report["commands"].append(record)
        return record

    def ff(self, arguments: list[str]):
        result = self.command([self.ffmpeg, "-hide_banner", "-loglevel", "error", "-nostdin", "-n", *arguments], 120)
        if result["returncode"] != 0:
            raise CaseFailure(f"ffmpeg failed; see command {len(self.report['commands']) - 1}")

    def encode(self, source: Path, target: Path, options: list[str], tags=None):
        metadata = []
        for key, value in (tags or {}).items():
            metadata.extend(["-metadata", f"{key}={value}"])
        self.ff(["-i", str(source), "-map", "0:a:0", "-map_metadata", "-1", *options, *metadata, str(target)])

    def encode_format(self, name: str, source: Path, target: Path, options: list[str]):
        if name == "mp3_lame":
            command = ["lame", "--silent", "-b", "192", str(source), str(target)]
        elif name == "vorbis":
            # FFmpeg's experimental native encoder did not preserve the source
            # lengths in the initial run. Use libvorbis through GStreamer.
            def location(path):
                return 'location="' + str(path).replace("\\", "\\\\").replace('"', '\\"') + '"'
            command = ["gst-launch-1.0", "-q", "filesrc", location(source),
                       "!", "wavparse", "!", "audioconvert", "!", "vorbisenc",
                       "quality=0.6", "!", "oggmux", "!", "filesink", location(target)]
        else:
            self.encode(source, target, options)
            return
        result = self.command(command, 120)
        if result["returncode"] != 0:
            raise CaseFailure(f"{name} encoding failed; see command {len(self.report['commands']) - 1}")

    def decode(self, source: Path, target: Path, mono: bool = False) -> array:
        # Explicit unity mono duplication matches the desired output contract;
        # ffmpeg's default mono->stereo matrix would instead attenuate by sqrt(2).
        filters = ["-af", "pan=stereo|c0=c0|c1=c0"] if mono else []
        self.ff(["-i", str(source), "-map", "0:a:0", *filters, "-ar", str(RATE),
                 "-ac", "2", "-c:a", "pcm_f32le", "-f", "f32le", str(target)])
        return load_pcm(target)

    def probe_run(self, paths: list[Path], output: Path | str, mode="off", transport=False, queue=None) -> tuple[dict, dict | None]:
        argv = [self.probe, "--output", str(output), "--gain", mode]
        if transport:
            argv.append("--exercise-transport")
        if self.clocked:
            argv.append("--clocked")
        if queue is not None:
            argv.extend(["--exercise-queue", queue])
        result = self.command([*argv, *(str(path.resolve()) for path in paths)])
        summary = None
        lines = result["stdout"].strip().splitlines()
        if lines:
            try:
                parsed = json.loads(lines[-1])
                if isinstance(parsed, dict):
                    summary = parsed
            except json.JSONDecodeError:
                pass
        return result, summary

    @staticmethod
    def healthy(result: dict, summary: dict | None, streams: int, transport=False) -> bool:
        return bool(result["returncode"] == 0 and not result["timed_out"]
                    and summary is not None and summary.get("streams") == streams
                    and summary.get("eos") is True and summary.get("errors") == []
                    and summary.get("transport_exercised") is transport
                    and summary.get("underruns") == 0
                    and isinstance(summary.get("gain_db"), list)
                    and all(isinstance(gain, (int, float)) and math.isfinite(gain)
                            for gain in summary["gain_db"]))

    def case(self, name: str, category: str, function):
        case = {"name": name, "category": category, "required": True,
                "command_start": len(self.report["commands"])}
        print(f"Running {name}", file=sys.stderr, flush=True)
        try:
            function(case)
        except (CaseFailure, OSError, ValueError, OverflowError, wave.Error) as error:
            case.update(passed=False, error=str(error))
        case["command_end_exclusive"] = len(self.report["commands"])
        case.setdefault("passed", False)
        self.report["cases"].append(case)
        self.save()

    def save(self):
        (self.directory / "report.json").write_text(
            json.dumps(self.report, indent=2, allow_nan=False) + "\n", encoding="utf-8")

    def format_case(self, name: str, case: dict):
        extension, options, lossless = FORMATS[name]
        folder = self.directory / name
        folder.mkdir()
        paths = []
        source = array("f")
        reference = array("f")
        reference_lengths = []
        reference_parts = []
        frame = 0
        for index, length in enumerate(PART_FRAMES):
            wav_path = folder / f"source-{index}.wav"
            source.extend(make_wav(wav_path, length, frame))
            path = folder / f"encoded-{index}.{extension}"
            self.encode_format(name, wav_path, path, options)
            decoded = self.decode(path, folder / f"reference-{index}.f32")
            reference.extend(decoded)
            reference_parts.append(decoded)
            reference_lengths.append(len(decoded) // 2)
            paths.append(path)
            frame += length
        self.format_paths[name] = paths
        capture = folder / "capture.f32"
        result, summary = self.probe_run(paths, capture)
        case.update(probe_summary=summary, source_part_frames=list(PART_FRAMES),
                    ffmpeg_decoded_part_frames=reference_lengths, lossless=lossless,
                    capture=str(capture), probe_ok=self.healthy(result, summary, len(paths)))
        actual = load_pcm(capture)
        source_error = comparison(actual, source)
        reference_error = comparison(actual, reference)
        positions = [sum(PART_FRAMES[:index]) for index in range(1, len(PART_FRAMES))]
        seams = boundaries(actual, source, positions)
        # Source continuity is diagnostic for lossy codecs: independent encoding
        # itself can introduce a boundary artifact. Judge handoff against the
        # decoded reference, without shifting or trimming either signal.
        reference_seams = boundaries(actual, reference, positions)
        for seam in (seams if lossless else reference_seams):
            if seam["available"]:
                seam["passed"] = (seam["delta_error"] <= (LOSSLESS_EPSILON * 2 if lossless else LOSSY_SEAM_ERROR)
                                  and seam["longest_near_zero_run_frames"] < 16
                                  and seam["window"]["rms_error"] <= (LOSSLESS_EPSILON if lossless else LOSSY_REFERENCE_RMS))
        source_ok = comparison_ok(source_error, LOSSLESS_EPSILON if lossless else LOSSY_SOURCE_RMS,
                                  LOSSLESS_EPSILON if lossless else LOSSY_SOURCE_MAX)
        # Compare independently decoded parts at their ORIGINAL source boundaries.
        # ffmpeg's padded tails must not move the start of subsequent comparisons.
        # No leading samples are removed and no measured offset is compensated.
        # The concatenated reference comparison remains visible as a diagnostic.
        part_comparisons = []
        offset = 0
        for length, decoded in zip(PART_FRAMES, reference_parts):
            error = comparison(actual[offset * 2:(offset + length) * 2], decoded)
            error["source_start_frame"] = offset
            error["passed"] = (error["compared_frames"] > 0
                               and error["rms_error"] <= (LOSSLESS_EPSILON if lossless else LOSSY_REFERENCE_RMS)
                               and error["max_abs_error"] <= (LOSSLESS_EPSILON if lossless else LOSSY_REFERENCE_MAX))
            part_comparisons.append(error)
            offset += length
        reference_ok = all(error["passed"] for error in part_comparisons)
        case.update(source_comparison=source_error, ffmpeg_comparison=reference_error,
                    source_comparison_passed=source_ok, ffmpeg_sample_comparison_passed=reference_ok,
                    ffmpeg_independent_part_comparisons=part_comparisons,
                    ffmpeg_reference_length_matches_source=len(reference) == len(source),
                    boundaries=seams, decoded_reference_boundaries=reference_seams,
                    classification="exact-lossless" if lossless else "lossy-unshifted-with-delay-padding-accounting",
                    passed=case["probe_ok"] and source_ok and reference_ok
                    and all(seam.get("passed", False) for seam in (seams if lossless else reference_seams)))
        if not case["passed"]:
            case["diagnostic_lags"] = [diagnostic_lag(actual, source, position)
                                       for position in [0, *positions]]
        independent = array("f")
        single_summaries = []
        for index, path in enumerate(paths):
            single_capture = folder / f"single-{index}.f32"
            single_result, single_summary = self.probe_run([path], single_capture)
            single_summaries.append(single_summary)
            if not self.healthy(single_result, single_summary, 1):
                raise CaseFailure("independent single-track decode failed")
            independent.extend(load_pcm(single_capture))
        case.update(single_track_summaries=single_summaries,
                    handoff_vs_independent_same_decoder=comparison(actual, independent))

    def gain_case(self, case: dict):
        folder = self.directory / "replaygain"
        folder.mkdir()
        frames = 24017
        wav_path = folder / "source.wav"
        source = make_wav(wav_path, frames)
        settings = [(-6.0, -3.0, 0.5), (3.0, -9.0, 0.5), (24.0, 24.0, 0.5), None]
        paths = []
        for index, setting in enumerate(settings):
            path = folder / f"gain-{index}.flac"
            tags = None
            if setting:
                track, album, peak = setting
                tags = {"REPLAYGAIN_TRACK_GAIN": f"{track:+.2f} dB",
                        "REPLAYGAIN_ALBUM_GAIN": f"{album:+.2f} dB",
                        "REPLAYGAIN_TRACK_PEAK": str(peak),
                        "REPLAYGAIN_ALBUM_PEAK": str(peak)}
            self.encode(wav_path, path, ["-c:a", "flac"], tags)
            paths.append(path)
        case.update(fixture_tags=settings, untagged_fallback_db=0.0,
                    peak_policy="min(requested gain, -20 log10(tagged peak)); protection enabled",
                    modes=[])
        all_ok = True
        source_rms = rms(source)
        for mode in ("off", "track", "album"):
            capture = folder / f"{mode}.f32"
            result, summary = self.probe_run(paths, capture, mode)
            try:
                actual = load_pcm(capture)
            except (CaseFailure, OSError, ValueError) as error:
                case["modes"].append({"mode": mode, "probe_summary": summary,
                                      "capture": str(capture), "passed": False, "error": str(error)})
                all_ok = False
                continue
            mode_result = {"mode": mode, "probe_summary": summary, "segments": [],
                           "frames": len(actual) // 2, "expected_frames": frames * len(paths),
                           "probe_ok": self.healthy(result, summary, len(paths)),
                           "capture": str(capture)}
            mode_ok = mode_result["probe_ok"] and len(actual) == frames * 2 * len(paths)
            expected_pcm = array("f")
            for index, setting in enumerate(settings):
                requested = 0.0 if mode == "off" or setting is None else setting[0 if mode == "track" else 1]
                expected_db = requested if mode == "off" or setting is None else min(requested, -20 * math.log10(setting[2]))
                factor = 10 ** (expected_db / 20)
                expected_segment = array("f", (value * factor for value in source))
                expected_pcm.extend(expected_segment)
                segment = actual[index * frames * 2:(index + 1) * frames * 2]
                measured_rms = rms(segment)
                measured_db = 20 * math.log10(measured_rms / source_rms) if measured_rms > 0 else None
                error = comparison(segment, expected_segment)
                passed = (measured_db is not None and abs(measured_db - expected_db) <= GAIN_DB_TOLERANCE
                          and comparison_ok(error, 0.0002, 0.002))
                mode_result["segments"].append({"index": index, "untagged": setting is None,
                                                "requested_db": requested, "expected_effective_db": expected_db,
                                                "measured_db": measured_db, "rms": measured_rms,
                                                "sample_comparison": error, "passed": passed})
                mode_ok = mode_ok and passed
            mode_result["boundaries"] = boundaries(actual, expected_pcm, [frames * index for index in range(1, len(paths))])
            mode_result["passed"] = bool(mode_ok)
            case["modes"].append(mode_result)
            all_ok = all_ok and mode_ok
        off_mode = next(mode for mode in case["modes"] if mode["mode"] == "off")
        for mode in case["modes"]:
            for segment, off_segment in zip(mode.get("segments", []), off_mode.get("segments", [])):
                segment["measured_relative_to_off_db"] = (
                    20 * math.log10(segment["rms"] / off_segment["rms"])
                    if segment["rms"] > 0 and off_segment["rms"] > 0 else None
                )
        case["passed"] = bool(all_ok)

    def mixed_case(self, case: dict):
        folder = self.directory / "mixed"
        folder.mkdir()
        specifications = [(44100, 1, 44117), (48000, 2, 48137),
                          (44100, 2, 33091), (48000, 1, 24013)]
        paths = []
        expected = array("f")
        positions = []
        case["parts"] = []
        for index, (rate, channels, frames) in enumerate(specifications):
            wav_path = folder / f"source-{index}.wav"
            make_wav(wav_path, frames, rate=rate, channels=channels)
            path = folder / f"encoded-{index}.flac"
            self.encode(wav_path, path, ["-c:a", "flac"])
            reference = self.decode(path, folder / f"reference-{index}.f32", mono=channels == 1)
            if index:
                positions.append(len(expected) // 2)
            expected.extend(reference)
            paths.append(path)
            case["parts"].append({"rate": rate, "channels": channels, "source_frames": frames,
                                  "reference_output_frames": len(reference) // 2})
        capture = folder / "capture.f32"
        result, summary = self.probe_run(paths, capture)
        actual = load_pcm(capture)
        errors = comparison(actual, expected)
        seams = boundaries(actual, expected, positions)
        for seam in seams:
            seam["passed"] = (seam["available"] and seam["window"]["rms_error"] <= 0.006
                              and seam["delta_error"] <= 0.05
                              and seam["longest_near_zero_run_frames"] < 16)
        case.update(probe_summary=summary, capture=str(capture), comparison=errors,
                    boundaries=seams,
                    classification="mixed-rate/channel conversion, not exact gapless certification",
                    mono_reference_matrix="unity duplication into left and right",
                    passed=self.healthy(result, summary, len(paths))
                    and comparison_ok(errors, 0.006, 0.05, frame_tolerance=2 * len(paths))
                    and all(seam["passed"] for seam in seams))

    def short_case(self, case: dict):
        folder = self.directory / "short"
        folder.mkdir()
        lengths = (48137, 137, 33611)
        expected = array("f")
        paths = []
        frame = 0
        for index, length in enumerate(lengths):
            wav_path = folder / f"source-{index}.wav"
            expected.extend(make_wav(wav_path, length, frame))
            path = folder / f"encoded-{index}.flac"
            self.encode(wav_path, path, ["-c:a", "flac"])
            paths.append(path)
            frame += length
        capture = folder / "capture.f32"
        result, summary = self.probe_run(paths, capture)
        actual = load_pcm(capture)
        errors = comparison(actual, expected)
        case.update(probe_summary=summary, source_part_frames=list(lengths), capture=str(capture),
                    comparison=errors, boundaries=boundaries(actual, expected, [lengths[0], sum(lengths[:2])]),
                    passed=self.healthy(result, summary, len(paths))
                    and comparison_ok(errors, LOSSLESS_EPSILON, LOSSLESS_EPSILON))

    def corrupt_case(self, case: dict):
        folder = self.directory / "corrupt-next"
        folder.mkdir()
        first = folder / "valid.wav"
        make_wav(first, 48137)
        broken = folder / "corrupt.flac"
        broken.write_bytes(b"fLaC\x00\x00\x00\x22" + b"deliberately-invalid-audio" * 17)
        result, summary = self.probe_run([first, broken], folder / "capture.f32")
        case.update(probe_summary=summary,
                    expected="bounded nonzero exit and visible errors after a valid first stream",
                    elapsed_seconds=result["elapsed_seconds"],
                    passed=(result["returncode"] is not None and result["returncode"] != 0
                            and not result["timed_out"] and summary is not None
                            and isinstance(summary.get("streams"), int) and summary["streams"] >= 1
                            and isinstance(summary.get("errors"), list) and bool(summary["errors"])
                            and all(isinstance(error, str) and error for error in summary["errors"])))

    def transport_case(self, case: dict, coreaudio=False):
        folder = self.directory / ("transport-coreaudio" if coreaudio else "transport-pcm")
        folder.mkdir()
        path = folder / "long.wav"
        make_wav(path, RATE * 8 + 137)
        output = "coreaudio" if coreaudio else folder / "capture.f32"
        result, summary = self.probe_run([path], output, transport=True)
        passed = self.healthy(result, summary, 1, transport=True)
        case.update(probe_summary=summary, output=str(output),
                    limitation="seek/pause intentionally break source continuity; no gapless claim")
        if not coreaudio:
            actual = load_pcm(output)
            case.update(captured_frames=len(actual) // 2, rms=rms(actual))
            passed = passed and len(actual) > RATE * 2 and rms(actual) > 0.005
        case["passed"] = bool(passed)

    def queue_case(self, case: dict, operation: str):
        paths = self.format_paths.get("flac")
        if paths is None:
            raise CaseFailure("FLAC fixtures unavailable for queue mutation")
        order = [0, 2] if operation == "remove-next" else [0, 2, 1]
        expected = array("f")
        for index in order:
            expected.extend(load_pcm(self.directory / "flac" / f"reference-{index}.f32"))
        capture = self.directory / f"queue-{operation}.f32"
        result, summary = self.probe_run(paths, capture, queue=operation)
        actual = load_pcm(capture)
        measured = comparison(actual, expected)
        case.update(probe_summary=summary, operation=operation, comparison=measured,
                    expected_entry_ids=[index + 1 for index in order],
                    passed=self.healthy(result, summary, len(order))
                    and summary.get("queue_edit_exercised") is True
                    and summary.get("started_entries") == [index + 1 for index in order]
                    and comparison_ok(measured, LOSSLESS_EPSILON, LOSSLESS_EPSILON))

    def device_case(self, name: str, case: dict):
        if name not in self.format_paths:
            raise CaseFailure("format fixture preparation failed; unavailable codec is not a pass")
        paths = self.format_paths[name]
        result, summary = self.probe_run(paths, "coreaudio")
        case.update(probe_summary=summary, output="coreaudio",
                    passed=self.healthy(result, summary, len(paths)),
                    human_listening_confirmed=False)

    def run(self, coreaudio: bool) -> int:
        self.command([self.ffmpeg, "-version"], 15)
        self.command([self.ffmpeg, "-hide_banner", "-encoders"], 15)
        self.command(["lame", "--version"], 15)
        self.command([self.ffmpeg, "-hide_banner", "-decoders"], 15)
        gst_inspect = shutil.which("gst-inspect-1.0")
        if gst_inspect:
            self.command([gst_inspect, "--version"], 15)
            self.command([gst_inspect, "vorbisenc"], 15)
        for name in FORMATS:
            self.case(f"same-rate-{name}", "numerical", lambda case, name=name: self.format_case(name, case))
        self.case("replaygain-modes-transitions-peak-fallback", "numerical", self.gain_case)
        self.case("mixed-rate-channel-transitions", "numerical", self.mixed_case)
        self.case("short-next-track", "numerical", self.short_case)
        self.case("corrupt-next-track", "numerical", self.corrupt_case)
        self.case("transport-pcm", "numerical", self.transport_case)
        self.case("remove-prepared-next", "numerical", lambda case: self.queue_case(case, "remove-next"))
        self.case("move-prepared-next", "numerical", lambda case: self.queue_case(case, "move-next"))
        if coreaudio:
            print("CoreAudio checks will emit modest-amplitude audio; automated success does not record listening.", file=sys.stderr)
            for name in FORMATS:
                self.case(f"coreaudio-{name}", "device", lambda case, name=name: self.device_case(name, case))
            self.case("transport-coreaudio", "device", lambda case: self.transport_case(case, coreaudio=True))
        cases = self.report["cases"]
        numerical = all(case["passed"] for case in cases if case["category"] == "numerical")
        device = all(case["passed"] for case in cases if case["category"] == "device") if coreaudio else None
        passed = numerical and device is not False
        self.report.update(
            finished_utc=datetime.now(timezone.utc).isoformat(),
            numerical_gate_passed=numerical,
            device_gate={"requested": coreaudio, "passed": device},
            human_listening={"status": "not-confirmed", "required_for_full_spike_gate": True},
            queue_edit_during_preparation={
                "status": "passed" if all(case["passed"] for case in cases if case["name"] in ("remove-prepared-next", "move-prepared-next")) else "failed",
                "required_for_full_spike_gate": True,
            },
            full_spike_audio_gate_passed=False,
            requested_automated_checks_passed=passed,
            automated_audio_gate_passed=passed,
            failed_cases=[case["name"] for case in cases if not case["passed"]],
        )
        self.save()
        print(json.dumps({"report": str(self.directory / "report.json"),
                          "numerical_gate_passed": numerical, "device_gate_passed": device,
                          "full_spike_audio_gate_passed": False,
                          "failed_cases": self.report["failed_cases"]}))
        return 0 if passed else 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--probe", default="target/debug/audio-probe", help="built probe executable")
    parser.add_argument("--work-dir", default="target/audio-gate", help="parent directory for isolated run artifacts")
    parser.add_argument("--coreaudio", action="store_true", help="also exercise actual CoreAudio playback (audible)")
    parser.add_argument("--clocked", action="store_true", help="pace PCM captures in real time to distinguish timing-dependent failures")
    args = parser.parse_args()
    gate = Gate(args)
    try:
        return gate.run(args.coreaudio)
    except KeyboardInterrupt:
        gate.report.update(interrupted=True, requested_automated_checks_passed=False,
                           full_spike_audio_gate_passed=False)
        gate.save()
        print(f"Interrupted; partial report: {gate.directory / 'report.json'}", file=sys.stderr)
        return 130


if __name__ == "__main__":
    raise SystemExit(main())
