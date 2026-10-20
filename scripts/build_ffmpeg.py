#!/usr/bin/env python3
"""Build pinned, LGPL-only native FFmpeg; stdout is the absolute install prefix."""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import tempfile


VERSION = "9.0.2"
ARCHIVE = f"ffmpeg-{VERSION}.tar.gz"
SOURCE_URL = f"https://ffmpeg.org/releases/{ARCHIVE}"
SOURCE_SHA256 = "84960df915059e8754fef2cd7c9afeb614062b1b5458ec471eecee619ee04e98"
# Verified against the detached upstream signature in an isolated GnuPG keyring.
# Fingerprint independently matches https://ffmpeg.org/download.html.
SIGNER = "FCF986EA15E6E293A5644F10B4322F04D67658D8"
MINIMUM_OS = "14.0"
LIBRARIES = {"avcodec": 63, "avformat": 63, "avutil": 61, "swresample": 7}
# DECODERS and DEMUXERS are post-configure assertions, not an allowlist.
# Configure enables every LGPL decoder and demuxer; require_configuration only
# checks that these are among them. Each scanner extension (AUDIO_EXTENSIONS in
# src/library_types.rs) maps to the demuxers and decoders it needs. An
# allowlist build would pass exactly these lists with --disable-decoders
# --disable-demuxers. Verify such a build with tests/ffmpeg_formats.rs against
# the new prefix; that file's header has the command.
PCM = ("pcm_s8 pcm_u8 pcm_s16le pcm_s16be pcm_s24le pcm_s24be pcm_s32le pcm_s32be "
       "pcm_f32le pcm_f32be pcm_f64le pcm_f64be pcm_alaw pcm_mulaw")
DSD = "dsd_lsbf dsd_msbf dsd_lsbf_planar dsd_msbf_planar"
EXTENSIONS = {  # extension: (demuxers, decoders)
    "aac": ("aac", "aac"),
    "ac3": ("ac3 eac3", "ac3 eac3"),
    "aif": ("aiff", f"{PCM} adpcm_ima_qt"),
    "aiff": ("aiff", f"{PCM} adpcm_ima_qt"),
    "aifc": ("aiff", f"{PCM} adpcm_ima_qt"),
    "alac": ("mov", "alac"),
    "amr": ("amr amrnb", "amrnb amrwb"),
    "ape": ("ape", "ape"),
    "au": ("au", PCM),
    "awb": ("amr amrwb", "amrwb"),
    "caf": ("caf", f"{PCM} adpcm_ima_qt alac aac"),
    "dff": ("iff", DSD),
    "dsf": ("dsf", DSD),
    "dts": ("dts dtshd", "dca"),
    "flac": ("flac", "flac"),
    "m4a": ("mov", "aac alac"),
    "m4b": ("mov", "aac alac"),
    "mka": ("matroska", f"flac opus vorbis mp3 aac ac3 eac3 {PCM}"),
    "mov": ("mov", f"aac alac {PCM} adpcm_ima_qt"),
    "mp2": ("mp3", "mp2"),
    "mp3": ("mp3", "mp3"),
    "mp4": ("mov", "aac alac mp3 opus flac ac3 eac3"),
    "mpc": ("mpc mpc8", "mpc7 mpc8"),
    "oga": ("ogg", "vorbis flac opus speex"),
    "ogg": ("ogg", "vorbis flac opus speex"),
    "opus": ("ogg", "opus"),
    "ra": ("rm", "ra_144 ra_288 cook sipr"),
    "shn": ("shorten", "shorten"),
    "spx": ("ogg", "speex"),
    "tak": ("tak", "tak"),
    "tta": ("tta", "tta"),
    "wav": ("wav", f"{PCM} adpcm_ima_wav adpcm_ms"),
    "wave": ("wav", f"{PCM} adpcm_ima_wav adpcm_ms"),
    "webm": ("matroska", "opus vorbis"),
    "wma": ("asf", "wmav1 wmav2 wmapro wmalossless wmavoice"),
    "wv": ("wv", "wavpack"),
}
COVER_ART_DECODERS = "png mjpeg"
DECODERS = sorted({name for _, names in EXTENSIONS.values() for name in names.split()}
                  | set(COVER_ART_DECODERS.split()))
DEMUXERS = sorted({name for names, _ in EXTENSIONS.values() for name in names.split()})


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def run(command, env, cwd=None, capture=False):
    result = subprocess.run([str(value) for value in command], env=env, cwd=cwd,
                            check=True, text=True,
                            stdout=subprocess.PIPE if capture else sys.stderr,
                            stderr=subprocess.STDOUT if capture else sys.stderr)
    return result.stdout.strip() if capture else None


def safe_build_path(path):
    # Upstream configure rejects whitespace in out-of-tree source paths; its
    # generated Makefiles also interpolate prefix/toolchain paths unescaped.
    if not re.fullmatch(r"[A-Za-z0-9_./+\-]+", str(path)):
        raise RuntimeError(f"FFmpeg's build system requires paths without whitespace or shell/make metacharacters: {path}. Move the checkout or select an Apple toolchain at a safe path before building.")


def toolchain(env):
    developer = run(["/usr/bin/xcode-select", "--print-path"], env, capture=True)
    safe_build_path(developer)
    env["DEVELOPER_DIR"] = developer
    tools = {}
    for name in ("clang", "clang++", "ar", "ranlib", "nm", "strip", "ld"):
        tools[name] = run(["/usr/bin/xcrun", "--sdk", "macosx", "--find", name], env, capture=True)
        safe_build_path(tools[name])
    sdk = run(["/usr/bin/xcrun", "--sdk", "macosx", "--show-sdk-path"], env, capture=True)
    safe_build_path(sdk)
    compiler = run([tools["clang"], "--version"], env, capture=True)
    if "Apple clang" not in compiler:
        raise RuntimeError("The selected toolchain must provide Apple clang; select Xcode or Command Line Tools with xcode-select.")
    env.update(SDKROOT=sdk, MACOSX_DEPLOYMENT_TARGET=MINIMUM_OS)
    return {
        "developer_directory": developer,
        "tools": tools,
        "compiler_version": compiler,
        "compiler_sha256": digest(Path(tools["clang"])),
        "linker_version": run([tools["ld"], "-v"], env, capture=True),
        "sdk_path": sdk,
        "sdk_version": run(["/usr/bin/xcrun", "--sdk", "macosx", "--show-sdk-version"], env, capture=True),
        "sdk_build_version": run(["/usr/bin/xcrun", "--sdk", "macosx", "--show-sdk-build-version"], env, capture=True),
        "make_version": run(["/usr/bin/make", "--version"], env, capture=True),
    }


def configure_options(tools):
    compiler_flags = f"-arch arm64 -mmacosx-version-min={MINIMUM_OS}"
    return [
        "--arch=aarch64", "--target-os=darwin", f"--sysroot={tools['sdk_path']}",
        f"--cc={tools['tools']['clang']}", f"--cxx={tools['tools']['clang++']}",
        f"--objcc={tools['tools']['clang']}", f"--as={tools['tools']['clang']}",
        f"--ld={tools['tools']['clang']}", f"--ar={tools['tools']['ar']}",
        f"--ranlib={tools['tools']['ranlib']}", f"--nm={tools['tools']['nm']}",
        f"--strip={tools['tools']['strip']}", "--pkg-config=/usr/bin/false",
        f"--extra-cflags={compiler_flags}", f"--extra-ldflags={compiler_flags} -Wl,-headerpad_max_install_names",
        # With autodetection disabled, FFmpeg only probes libc for iconv and
        # skips its -liconv fallback. macOS provides it in a separate system lib.
        "--extra-libs=-liconv",
        "--enable-shared", "--disable-static", "--enable-pic",
        "--disable-gpl", "--disable-nonfree", "--disable-version3",
        "--disable-autodetect", "--enable-zlib", "--enable-bzlib", "--enable-iconv",
        "--disable-programs", "--disable-doc", "--disable-debug", "--disable-checkasm",
        "--disable-avdevice", "--disable-avfilter", "--disable-swscale",
        "--disable-devices", "--disable-filters", "--disable-encoders", "--disable-muxers",
        "--disable-hwaccels", "--disable-network",
    ]


def require_configuration(build):
    components = (build / "config_components.h").read_text()
    config = (build / "config.h").read_text()
    for kind, names in (("DECODER", DECODERS), ("DEMUXER", DEMUXERS)):
        for name in names:
            if f"#define CONFIG_{name.upper()}_{kind} 1" not in components:
                raise RuntimeError(f"Required native FFmpeg {kind.lower()} missing: {name}")
    for name in ("GPL", "NONFREE", "VERSION3"):
        if f"#define CONFIG_{name} 0" not in config:
            raise RuntimeError(f"FFmpeg licensing configuration unexpectedly enabled {name}")
    for name in ("ZLIB", "BZLIB", "ICONV", "AVCODEC", "AVFORMAT", "AVUTIL", "SWRESAMPLE"):
        if f"#define CONFIG_{name} 1" not in config:
            raise RuntimeError(f"Required FFmpeg configuration missing: {name}")


def installed_files(prefix):
    files = {}
    for path in sorted(prefix.rglob("*")):
        if path.name == ".complete.json":
            continue
        relative = str(path.relative_to(prefix))
        if path.is_symlink():
            if not path.resolve().is_relative_to(prefix):
                raise RuntimeError(f"Installed FFmpeg symlink escapes prefix: {path}")
            files[relative] = {"symlink": os.readlink(path)}
        elif path.is_file():
            files[relative] = {"sha256": digest(path)}
    return files


def reusable(prefix, identity):
    try:
        complete = json.loads((prefix / ".complete.json").read_text())
        return (complete["identity"] == identity and bool(complete["files"])
                and complete["files"] == installed_files(prefix))
    except (OSError, ValueError, KeyError, RuntimeError):
        return False


def download(archive, env):
    if not archive.exists():
        temporary = archive.with_suffix(archive.suffix + ".partial")
        try:
            run(["/usr/bin/curl", "--disable", "--fail", "--location", "--proto", "=https",
                 "--proto-redir", "=https", "--tlsv1.2", "--connect-timeout", "30",
                 "--max-time", "600", "--output", temporary, SOURCE_URL], env)
            if digest(temporary) != SOURCE_SHA256:
                raise RuntimeError("Downloaded FFmpeg source does not match the pinned SHA256; refusing to extract it.")
            temporary.replace(archive)
        finally:
            temporary.unlink(missing_ok=True)
    if digest(archive) != SOURCE_SHA256:
        raise RuntimeError(f"Cached FFmpeg source checksum mismatch: {archive}. Remove this archive and run again.")


def build(prefix, archive, identity, options, env, root):
    # Only the final completion record makes an install reusable. Interrupted
    # installs are removed on the next invocation, under the same exclusive lock.
    if prefix.exists():
        shutil.rmtree(prefix)
    with tempfile.TemporaryDirectory(prefix="ffmpeg-work-", dir=root) as directory:
        work = Path(directory)
        run(["/usr/bin/tar", "-xzf", archive, "-C", work], env)
        source = work / f"ffmpeg-{VERSION}"
        build_dir = work / "build"
        build_dir.mkdir()
        config = [f"--prefix={prefix}", *options]
        try:
            run(["/bin/sh", source / "configure", *config], env, cwd=build_dir)
        except subprocess.CalledProcessError:
            log = build_dir / "ffbuild/config.log"
            if log.exists():
                print("\nFFmpeg configure log (last 120 lines):", file=sys.stderr)
                print("\n".join(log.read_text(errors="replace").splitlines()[-120:]), file=sys.stderr)
            raise
        require_configuration(build_dir)
        run(["/usr/bin/make", f"-j{os.cpu_count() or 1}"], env, cwd=build_dir)
        run(["/usr/bin/make", "install"], env, cwd=build_dir)
        for library, major in LIBRARIES.items():
            for name in (f"lib{library}.{major}.dylib", f"lib{library}.dylib"):
                if not (prefix / "lib" / name).is_file():
                    raise RuntimeError(f"FFmpeg install did not produce {name}")
            if not (prefix / "lib/pkgconfig" / f"lib{library}.pc").is_file():
                raise RuntimeError(f"FFmpeg install did not produce lib{library}.pc")
        provenance = prefix / "share/tyranny-ffmpeg"
        provenance.mkdir(parents=True)
        shutil.copyfile(archive, provenance / ARCHIVE)
        for name in ("COPYING.LGPLv2.1", "LICENSE.md"):
            shutil.copyfile(source / name, provenance / name)
        for name in ("config.h", "config_components.h", "ffbuild/config.mak"):
            shutil.copyfile(build_dir / name, provenance / Path(name).name)
        info = {
            **identity,
            "configure_arguments": config,
            "license": "LGPL-2.1-or-later",
            "source_modified": False,
            "source_authentication": {
                "method": "Pinned SHA256; pin established with GnuPG detached-signature verification in an isolated keyring",
                "signature_url": SOURCE_URL + ".asc",
                "signer_fingerprint": SIGNER,
                "key_url": "https://ffmpeg.org/ffmpeg-devel.asc",
                "fingerprint_reference": "https://ffmpeg.org/download.html",
            },
            "required_native_decoders": DECODERS,
            "required_demuxers": DEMUXERS,
            "environment": env,
        }
        (provenance / "build-info.json").write_text(json.dumps(info, indent=2) + "\n")
        complete = {"identity": identity, "files": installed_files(prefix)}
        marker = prefix / ".complete.json.partial"
        marker.write_text(json.dumps(complete, indent=2) + "\n")
        marker.replace(prefix / ".complete.json")


def main():
    if len(sys.argv) != 1:
        raise RuntimeError("Usage: python3 scripts/build_ffmpeg.py (no arguments)")
    if sys.platform != "darwin" or platform.machine() != "arm64":
        raise RuntimeError("Pinned FFmpeg builds require native arm64 macOS (run arm64 Python, not Rosetta).")
    root = (Path(__file__).resolve().parent.parent / "target/native").resolve()
    safe_build_path(root)
    # Start from an allowlist, not os.environ: no caller CFLAGS, LDFLAGS,
    # CPATH, LIBRARY_PATH, SDKROOT, PKG_CONFIG_*, MAKEFLAGS, or DYLD_* survive.
    env = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "HOME": str(Path.home()),
           "LANG": "C", "LC_ALL": "C", "TZ": "UTC", "TMPDIR": "/tmp"}
    tools = toolchain(env)
    options = configure_options(tools)
    identity = {
        "version": VERSION, "source_url": SOURCE_URL, "source_sha256": SOURCE_SHA256,
        "architecture": "arm64", "minimum_macos": MINIMUM_OS, "toolchain": tools,
        "configure_options": options, "builder_sha256": digest(Path(__file__)),
        "install_root": str(root),
    }
    key = hashlib.sha256(json.dumps(identity, sort_keys=True).encode()).hexdigest()[:24]
    prefix = root / f"ffmpeg-{VERSION}-arm64-{key}"
    root.mkdir(parents=True, exist_ok=True)
    with (root / ".ffmpeg-build.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if reusable(prefix, identity):
            print(f"Using pinned FFmpeg {VERSION}: {prefix}", file=sys.stderr)
        else:
            downloads = root / "downloads"
            downloads.mkdir(exist_ok=True)
            archive = downloads / ARCHIVE
            download(archive, env)
            print(f"Building pinned FFmpeg {VERSION}: {prefix}", file=sys.stderr)
            build(prefix, archive, identity, options, env, root)
    print(prefix)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        if isinstance(error, subprocess.CalledProcessError) and error.stdout:
            print(error.stdout, file=sys.stderr)
        print(f"FFmpeg build failed: {error}", file=sys.stderr)
        sys.exit(1)
