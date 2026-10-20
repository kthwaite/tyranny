#!/usr/bin/env python3
"""Check that the FFmpeg component assertions cover every scanner extension."""
import importlib.util
from pathlib import Path
import re
import unittest


SCRIPT = Path(__file__).with_name("build_ffmpeg.py")
LIBRARY_TYPES = Path(__file__).resolve().parent.parent / "src/library_types.rs"


def load_builder():
    spec = importlib.util.spec_from_file_location("build_ffmpeg", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def scanner_extensions():
    source = LIBRARY_TYPES.read_text()
    match = re.search(r"pub const AUDIO_EXTENSIONS: &\[&str\] = &\[(.*?)\];", source, re.S)
    if match is None:
        raise AssertionError(f"AUDIO_EXTENSIONS not found in {LIBRARY_TYPES}")
    return re.findall(r'"([^"]+)"', match.group(1))


class ComponentTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.builder = load_builder()

    def test_every_scanner_extension_has_components(self):
        extensions = scanner_extensions()
        self.assertEqual(len(extensions), len(set(extensions)))
        self.assertEqual(set(self.builder.EXTENSIONS), set(extensions))
        for extension, (demuxers, decoders) in self.builder.EXTENSIONS.items():
            self.assertTrue(demuxers.split(), extension)
            self.assertTrue(decoders.split(), extension)

    def test_lists_are_derived_from_extensions(self):
        demuxers = {name for names, _ in self.builder.EXTENSIONS.values() for name in names.split()}
        decoders = {name for _, names in self.builder.EXTENSIONS.values() for name in names.split()}
        self.assertEqual(self.builder.DEMUXERS, sorted(demuxers))
        self.assertEqual(self.builder.DECODERS, sorted(decoders | {"png", "mjpeg"}))

    def test_lists_include_verified_components(self):
        # Previous lists plus the additions a strict allowlist build needed to
        # scan and decode every fixture (TSK-aware-yarn spike).
        decoders = ("flac mp3 aac alac opus vorbis wavpack mpc7 mpc8 wmav1 wmav2 wmapro "
                    "wmalossless wmavoice ape tta shorten png mjpeg "
                    "pcm_s8 pcm_u8 pcm_s16le pcm_s16be pcm_s24le pcm_s24be pcm_s32le "
                    "pcm_s32be pcm_f32le pcm_f32be pcm_f64le pcm_f64be pcm_alaw pcm_mulaw "
                    "adpcm_ima_wav adpcm_ms adpcm_ima_qt ac3 eac3 dca mp2 amrnb amrwb speex "
                    "dsd_lsbf dsd_msbf dsd_lsbf_planar dsd_msbf_planar tak ra_144 ra_288 "
                    "cook sipr").split()
        demuxers = ("flac mp3 aac mov ogg wv mpc mpc8 asf ape tta shorten wav aiff matroska "
                    "ac3 eac3 dts dtshd amr amrnb amrwb caf au dsf iff rm tak").split()
        self.assertEqual(set(decoders) - set(self.builder.DECODERS), set())
        self.assertEqual(set(demuxers) - set(self.builder.DEMUXERS), set())

    def test_build_stays_permissive(self):
        tools = {"sdk_path": "/sdk", "tools": dict.fromkeys(
            ("clang", "clang++", "ar", "ranlib", "nm", "strip"), "/tool")}
        options = self.builder.configure_options(tools)
        for option in ("--disable-everything", "--disable-decoders", "--disable-demuxers"):
            self.assertNotIn(option, options)


if __name__ == "__main__":
    unittest.main()
