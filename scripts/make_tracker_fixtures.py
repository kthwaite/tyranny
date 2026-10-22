#!/usr/bin/env python3
"""Generate only the four self-authored tracker fixtures; preserve other fixtures.

    python3 scripts/make_tracker_fixtures.py
    python3 scripts/make_tracker_fixtures.py --libopenmpt <portable-prefix>/lib/libopenmpt.dylib

No music or samples are copied from third parties. Every module plays one note
from an integer triangle waveform, then jumps backwards at row 7. IT has two
subsongs with different notes separated by a stop order; each jumps to its own
start. At speed 6 / tempo 125 each is 0.96 s before libopenmpt's end fade. Song
loops must terminate with repeat count zero. Sample loops sustain each note.

Byte layouts follow the primary OpenMPT loaders and format structures:
https://github.com/OpenMPT/openmpt/blob/master/soundlib/Load_mod.cpp
https://github.com/OpenMPT/openmpt/blob/master/soundlib/XMTools.h
https://github.com/OpenMPT/openmpt/blob/master/soundlib/ITTools.h
https://github.com/OpenMPT/openmpt/blob/master/soundlib/S3MTools.h

Without --libopenmpt (or TYRANNY_LIKES_SMOKE_LIBOPENMPT), new/changed fixtures have null
reference frame counts, never invented counts. Finalize with the pinned library;
the independent C API render measures actual frames without FFmpeg CLI programs.
Existing references survive regeneration only when the fixture SHA-256 is
unchanged. Generic format tests require these references, so an unfinished
manifest cannot silently pass.
"""
import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import struct

ROOT = Path(__file__).resolve().parents[1] / "tests" / "fixtures"
EXTENSIONS = ("mod", "xm", "it", "s3m")
# One 32-sample signed 8-bit triangle period, peak 96; no float/platform rounding.
SAMPLE = bytes(((-96 + 12 * n) if n < 16 else (288 - 12 * n)) % 256
               for n in range(32))


def text(value, size):
    return value.encode("ascii").ljust(size, b"\0")


def mod(title):
    header = bytearray(text(title, 20))
    header += text("Tyranny triangle", 22) + struct.pack(">HBBHH", 16, 0, 64, 0, 16)
    for _ in range(30):
        header += bytes(28) + struct.pack(">H", 1)
    header += bytes([1, 0]) + bytes(128) + b"M.K."
    pattern = bytearray(64 * 4 * 4)
    # Sample 1, period 428 (C-3); B00 at row 7 repeats the song indefinitely
    # in a player that does not detect backward order loops.
    pattern[0:4] = bytes([0x01, 0xAC, 0x10, 0])
    pattern[7 * 16:7 * 16 + 4] = bytes([0, 0, 0x0B, 0])
    return bytes(header + pattern + SAMPLE)


def xm(title):
    header = b"Extended Module: " + text(title, 20) + b"\x1a"
    header += text("Tyranny fixtures", 20) + struct.pack("<HI8H", 0x0104, 276,
                                                                 1, 0, 4, 1, 1, 1, 6, 125)
    header += bytes(256)
    pattern = bytearray(8 * 4 * 5)
    pattern[0:5] = bytes([49, 1, 0, 0, 0])  # C-4, instrument 1
    pattern[7 * 20:7 * 20 + 5] = bytes([0, 0, 0, 0x0B, 0])
    pattern_header = struct.pack("<IBHH", 9, 0, 8, len(pattern))
    instrument = struct.pack("<I", 263) + text("Tyranny triangle", 22)
    instrument += struct.pack("<BHI", 0, 1, 40) + bytes(230)
    sample_header = struct.pack("<IIIBbBBbB", 32, 0, 32, 64, 0, 1, 128, 0, 0)
    sample_header += text("Tyranny triangle", 22)
    previous = 0
    deltas = bytearray()
    for value in SAMPLE:
        deltas.append((value - previous) % 256)
        previous = value
    return header + pattern_header + pattern + instrument + sample_header + deltas


def it(title):
    header = bytearray(192)
    header[:4] = b"IMPM"
    header[4:30] = text(title, 26)
    struct.pack_into("<8H", header, 32, 4, 0, 1, 2, 0x0214, 0x0214, 1, 0)
    header[48:54] = bytes([128, 48, 6, 125, 128, 0])
    header[64:128] = bytes([32]) + bytes([0xFF]) * 63
    header[128:192] = bytes([64]) * 64
    # Two subsongs: each has a pattern and stop order, with a backward jump.
    sample_offset = 192 + 4 + 4 + 8
    pattern_offset = sample_offset + 80
    patterns = []
    for note, jump_order in ((60, 0), (67, 2)):
        packed = (bytes([0x81, 3, note, 1, 0]) + bytes(6)
                  + bytes([0x81, 8, 2, jump_order, 0]))
        patterns.append(struct.pack("<HHI", len(packed), 8, 0) + packed)
    sample = bytearray(80)
    sample[:4] = b"IMPS"
    sample[17:20] = bytes([64, 0x11, 64])  # Present, forward sample loop.
    sample[20:46] = text("Tyranny triangle", 26)
    sample[46] = 1  # Signed PCM.
    struct.pack_into("<7I", sample, 48, 32, 0, 32, 8363, 0, 0,
                     pattern_offset + sum(map(len, patterns)))
    return (header + bytes([0, 255, 1, 255])
            + struct.pack("<III", sample_offset, pattern_offset, pattern_offset + len(patterns[0]))
            + sample + b"".join(patterns) + SAMPLE)


def s3m(title):
    header = bytearray(96)
    header[:28] = text(title, 28)
    header[28:30] = bytes([0x1A, 0x10])
    struct.pack_into("<6H", header, 32, 2, 1, 1, 0, 0x1320, 2)
    header[44:48] = b"SCRM"
    header[48:52] = bytes([64, 6, 125, 0xB0])
    header[64:96] = bytes([0]) + bytes([0xFF]) * 31
    sample_offset, pattern_offset, data_offset = 112, 192, 288
    sample = bytearray(80)
    sample[0] = 1
    sample[13:16] = bytes([0]) + struct.pack("<H", data_offset // 16)
    struct.pack_into("<III", sample, 16, 32, 0, 32)
    sample[28] = 64
    sample[31] = 1
    struct.pack_into("<I", sample, 32, 8363)
    sample[48:76] = text("Tyranny triangle", 28)
    sample[76:80] = b"SCRS"
    packed = bytes([0x20, 0x40, 1, 0]) + bytes(6) + bytes([0x80, 2, 0, 0]) + bytes(56)
    pattern = struct.pack("<H", len(packed)) + packed
    result = header + bytes([0, 255]) + struct.pack("<HH", sample_offset // 16,
                                                               pattern_offset // 16)
    result += bytes(sample_offset - len(result)) + sample + pattern
    result += bytes(data_offset - len(result))
    return result + bytes((value + 128) % 256 for value in SAMPLE)


class OpenMPTReference:
    """Independent 48 kHz stereo reference using the public libopenmpt C API.

    Signatures: https://lib.openmpt.org/doc/libopenmpt_8h.html
    Retain default repeat count zero and select all subsongs, as FFmpeg does.
    """

    def __init__(self, path):
        self.lib = ctypes.CDLL(str(Path(path).resolve()))
        pointer = ctypes.c_void_p
        signatures = {
            "openmpt_module_create_from_memory2": (pointer, [
                pointer, ctypes.c_size_t, pointer, pointer, pointer, pointer,
                ctypes.POINTER(ctypes.c_int), ctypes.POINTER(pointer), pointer]),
            "openmpt_module_destroy": (None, [pointer]),
            "openmpt_free_string": (None, [pointer]),
            "openmpt_get_string": (pointer, [ctypes.c_char_p]),
            "openmpt_module_get_metadata": (pointer, [pointer, ctypes.c_char_p]),
            "openmpt_module_select_subsong": (ctypes.c_int, [pointer, ctypes.c_int32]),
            "openmpt_module_get_repeat_count": (ctypes.c_int32, [pointer]),
            "openmpt_module_get_num_subsongs": (ctypes.c_int32, [pointer]),
            "openmpt_module_get_duration_seconds": (ctypes.c_double, [pointer]),
            "openmpt_module_error_get_last": (ctypes.c_int, [pointer]),
            "openmpt_module_read_interleaved_float_stereo": (ctypes.c_size_t, [
                pointer, ctypes.c_int32, ctypes.c_size_t, ctypes.POINTER(ctypes.c_float)]),
        }
        for name, (result, arguments) in signatures.items():
            function = getattr(self.lib, name)
            function.restype = result
            function.argtypes = arguments
        self.version = self.take_string(self.lib.openmpt_get_string(b"library_version"))

    def take_string(self, pointer):
        if not pointer:
            return ""
        try:
            return ctypes.string_at(pointer).decode("utf-8", errors="replace")
        finally:
            self.lib.openmpt_free_string(pointer)

    def measure(self, data, title, subsongs):
        source = ctypes.create_string_buffer(bytes(data))
        error = ctypes.c_int()
        message = ctypes.c_void_p()
        module = self.lib.openmpt_module_create_from_memory2(
            source, len(data), None, None, None, None,
            ctypes.byref(error), ctypes.byref(message), None)
        detail = self.take_string(message.value)
        if not module:
            raise RuntimeError(f"{title}: libopenmpt error {error.value}: {detail}")
        try:
            actual_title = self.take_string(self.lib.openmpt_module_get_metadata(module, b"title"))
            if actual_title != title:
                raise RuntimeError(f"{title}: unexpected embedded title {actual_title!r}")
            if self.lib.openmpt_module_get_num_subsongs(module) != subsongs:
                raise RuntimeError(f"{title}: expected {subsongs} subsongs")
            if not self.lib.openmpt_module_select_subsong(module, -1):
                raise RuntimeError(f"{title}: cannot select all subsongs")
            if self.lib.openmpt_module_get_repeat_count(module) != 0:
                raise RuntimeError(f"{title}: libopenmpt default repeat count is not zero")
            duration = self.lib.openmpt_module_get_duration_seconds(module)
            if not 0 < duration < 5:
                raise RuntimeError(f"{title}: unusable duration {duration}")
            # Match FFmpeg libopenmpt's 2048-byte stereo float packet size.
            buffer = (ctypes.c_float * (256 * 2))()
            frames = 0
            audible = False
            while True:
                count = self.lib.openmpt_module_read_interleaved_float_stereo(
                    module, 48000, 256, buffer)
                if self.lib.openmpt_module_error_get_last(module):
                    raise RuntimeError(f"{title}: libopenmpt read failed")
                if not count:
                    break
                frames += count
                if count > 256 or frames > 5 * 48000:
                    raise RuntimeError(f"{title}: malformed or unbounded decode")
                audible |= any(abs(buffer[index]) > 0.001 for index in range(count * 2))
            if not frames or not audible:
                raise RuntimeError(f"{title}: empty or silent decode")
            return frames, duration
        finally:
            self.lib.openmpt_module_destroy(module)


def add_fixtures(root, manifest, libopenmpt=None):
    root = Path(root)
    reference = OpenMPTReference(libopenmpt) if libopenmpt else None
    old = {entry["extension"]: entry for entry in manifest["formats"]["files"]
           if entry["extension"] in EXTENSIONS}
    entries = []
    (root / "trackers").mkdir(exist_ok=True)
    for extension, make in zip(EXTENSIONS, (mod, xm, it, s3m)):
        title = "Tyranny " + extension.upper() + " loop"
        data = make(title)
        digest = hashlib.sha256(data).hexdigest()
        relative = "trackers/loop." + extension
        path = root / relative
        path.write_bytes(data)
        prior = old.get(extension, {})
        frames = prior.get("frames") if prior.get("sha256") == digest else None
        entry = {"extension": extension, "file": relative, "codec": "pcm_f32le",
                 "rate": 48000, "channels": 2, "seconds": 1.92 if extension == "it" else 0.96,
                 "subsongs": 2 if extension == "it" else 1, "frames": frames,
                 "title": title, "sha256": digest,
                 "frames_from": "libopenmpt C API, 48000 Hz stereo float, subsong=-1, default repeat count 0"}
        if reference:
            entry["frames"], entry["duration_seconds"] = reference.measure(
                data, title, entry["subsongs"])
        elif prior.get("sha256") == digest and "duration_seconds" in prior:
            entry["duration_seconds"] = prior["duration_seconds"]
        entries.append(entry)
    if reference:
        manifest["encoders"]["tracker reference libopenmpt"] = reference.version
    manifest["encoders"]["mod, xm, it, s3m"] = (
        "scripts/make_tracker_fixtures.py; self-authored integer triangle, one note, "
        "backward order jump at row 7; no third-party music or samples")
    manifest["formats"]["files"] = [entry for entry in manifest["formats"]["files"]
                                      if entry["extension"] not in EXTENSIONS] + entries


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--libopenmpt", type=Path, default=os.environ.get("TYRANNY_LIKES_SMOKE_LIBOPENMPT"),
                        help="pinned libopenmpt shared library used to finalize reference frame counts")
    args = parser.parse_args()
    manifest_path = ROOT / "manifest.json"
    manifest = json.loads(manifest_path.read_text())
    add_fixtures(ROOT, manifest, args.libopenmpt)
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")


if __name__ == "__main__":
    main()
