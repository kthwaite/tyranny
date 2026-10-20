#!/usr/bin/env python3
"""Check the pinned build's protocol/TLS contract without compiling FFmpeg."""
from pathlib import Path
import tempfile
import unittest

import build_ffmpeg


class ConfigurationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="tyranny-ffmpeg-config-")
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
                "SWRESAMPLE", "NETWORK", "SECURETRANSPORT")]
            + [f"#define CONFIG_{name} 0" for name in ("GPL", "NONFREE", "VERSION3")]
        ) + "\n"

    def check_configuration(self):
        (self.root / "config_components.h").write_text(self.components)
        (self.root / "config.h").write_text(self.config)
        build_ffmpeg.require_configuration(self.root)

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


if __name__ == "__main__":
    unittest.main()
