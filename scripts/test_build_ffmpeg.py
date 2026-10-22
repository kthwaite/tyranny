#!/usr/bin/env python3
"""Check pinned native dependencies and protocol/TLS contracts without builds."""
import copy
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import build_ffmpeg


class ConfigurationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="tyranny-likes-smoke-ffmpeg-config-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.components = "\n".join(
            f"#define CONFIG_{name.upper()}_{kind} 1"
            for kind, names in (("DECODER", build_ffmpeg.DECODERS),
                                ("DEMUXER", build_ffmpeg.DEMUXERS),
                                ("PROTOCOL", build_ffmpeg.PROTOCOLS))
            for name in names
        ) + "\n"
        self.config = "\n".join(
            [f"#define CONFIG_{name} 1" for name in (
                "ZLIB", "BZLIB", "ICONV", "AVCODEC", "AVFORMAT", "AVUTIL",
                "SWRESAMPLE", "NETWORK", "SECURETRANSPORT", "LIBOPENMPT")]
            + [f"#define CONFIG_{name} 0" for name in ("GPL", "NONFREE", "VERSION3")]
        ) + "\n"

    def test_missing_openmpt_library_configuration_is_rejected(self):
        self.config = self.config.replace("CONFIG_LIBOPENMPT 1", "CONFIG_LIBOPENMPT 0")
        with self.assertRaisesRegex(RuntimeError, "LIBOPENMPT"):
            self.check_configuration()

    def check_configuration(self):
        (self.root / "config_components.h").write_text(self.components)
        (self.root / "config.h").write_text(self.config)
        build_ffmpeg.require_configuration(self.root)

    def test_missing_required_audio_components_are_rejected(self):
        original = self.components
        for kind, names in (("DECODER", build_ffmpeg.DECODERS),
                            ("DEMUXER", build_ffmpeg.DEMUXERS)):
            for name in names:
                with self.subTest(kind=kind, component=name):
                    flag = f"CONFIG_{name.upper()}_{kind}"
                    self.components = original.replace(f"{flag} 1", f"{flag} 0")
                    with self.assertRaisesRegex(RuntimeError, f"{kind.lower()} missing"):
                        self.check_configuration()

    def test_missing_https_or_tls_is_rejected(self):
        original = self.components
        for name in ("HTTPS", "TLS"):
            with self.subTest(protocol=name):
                self.components = original.replace(f"CONFIG_{name}_PROTOCOL 1",
                                                   f"CONFIG_{name}_PROTOCOL 0")
                with self.assertRaisesRegex(RuntimeError, "protocol missing"):
                    self.check_configuration()

    def test_unexpected_network_or_nested_protocol_is_rejected(self):
        original = self.components
        for name in ("UDP", "FTP", "CONCAT", "DATA"):
            with self.subTest(protocol=name):
                self.components = original + f"#define CONFIG_{name}_PROTOCOL 1\n"
                with self.assertRaisesRegex(RuntimeError, "Unexpected native FFmpeg protocols"):
                    self.check_configuration()

    def test_missing_system_tls_backend_is_rejected(self):
        self.config = self.config.replace("CONFIG_SECURETRANSPORT 1", "CONFIG_SECURETRANSPORT 0")
        with self.assertRaisesRegex(RuntimeError, "SECURETRANSPORT"):
            self.check_configuration()


class DependencyTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="tyranny-likes-smoke-native-config-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def test_cached_dependency_archive_requires_its_own_checksum(self):
        archive = self.root / "source.tar.gz"
        archive.write_bytes(b"pinned dependency")
        expected = hashlib.sha256(archive.read_bytes()).hexdigest()
        build_ffmpeg.download(archive, {}, "https://invalid.example/source", expected)
        archive.write_bytes(b"changed dependency")
        with self.assertRaisesRegex(RuntimeError, "checksum mismatch"):
            build_ffmpeg.download(archive, {}, "https://invalid.example/source", expected)

    def test_download_uses_mirror_after_connection_failure(self):
        archive = self.root / "source.tar.gz"
        partial = archive.with_suffix(".gz.partial")
        content = b"pinned dependency"
        expected = hashlib.sha256(content).hexdigest()
        primary = "https://upstream.example/source"
        mirror = "https://mirror.example/source"

        def fetch(command, env):
            if command[-1] == primary:
                partial.write_bytes(b"interrupted transfer")
                raise subprocess.CalledProcessError(7, command)
            self.assertFalse(partial.exists())
            Path(command[-2]).write_bytes(content)

        with patch.object(build_ffmpeg, "run", side_effect=fetch):
            build_ffmpeg.download(archive, {}, primary, expected, (mirror,))
        self.assertEqual(archive.read_bytes(), content)
        self.assertFalse(partial.exists())

    def test_failed_sources_leave_no_cached_or_partial_archive(self):
        archive = self.root / "source.tar.gz"
        partial = archive.with_suffix(".gz.partial")

        def fetch(command, env):
            partial.write_bytes(b"interrupted transfer")
            raise subprocess.CalledProcessError(7, command)

        with patch.object(build_ffmpeg, "run", side_effect=fetch):
            with self.assertRaises(subprocess.CalledProcessError):
                build_ffmpeg.download(archive, {}, "https://upstream.example/source",
                                      "0" * 64, ("https://mirror.example/source",))
        self.assertFalse(archive.exists())
        self.assertFalse(partial.exists())

    def test_checksum_mismatch_is_fatal_on_primary_and_mirror(self):
        archive = self.root / "source.tar.gz"
        partial = archive.with_suffix(".gz.partial")
        primary = "https://upstream.example/source"
        mirror = "https://mirror.example/source"
        for primary_down in (False, True):
            with self.subTest(primary_down=primary_down):
                def fetch(command, env):
                    if primary_down and command[-1] == primary:
                        raise subprocess.CalledProcessError(7, command)
                    Path(command[-2]).write_bytes(b"untrusted source")

                with patch.object(build_ffmpeg, "run", side_effect=fetch) as fetcher:
                    with self.assertRaisesRegex(RuntimeError, "pinned SHA256"):
                        build_ffmpeg.download(archive, {}, primary, "0" * 64, (mirror,))
                self.assertEqual(fetcher.call_count, 2 if primary_down else 1)
                self.assertFalse(archive.exists())
                self.assertFalse(partial.exists())

    def test_cache_rejects_dependency_pin_or_installed_library_changes(self):
        library = self.root / "libopenmpt.0.dylib"
        library.write_bytes(b"pinned library")
        identity = {"dependencies": copy.deepcopy(build_ffmpeg.DEPENDENCIES)}
        marker = {"identity": identity, "files": build_ffmpeg.installed_files(self.root)}
        (self.root / ".complete.json").write_text(json.dumps(marker))
        self.assertTrue(build_ffmpeg.reusable(self.root, identity))
        changed = copy.deepcopy(identity)
        changed["dependencies"]["libopenmpt"]["source_sha256"] = "0" * 64
        self.assertFalse(build_ffmpeg.reusable(self.root, changed))
        library.write_bytes(b"modified library")
        self.assertFalse(build_ffmpeg.reusable(self.root, identity))


if __name__ == "__main__":
    unittest.main()
