#!/usr/bin/env python3
"""Regenerate tests/fixtures/ (format and gapless test fixtures + manifest.json).

Usage: python3 scripts/make_test_fixtures.py

Needs ffmpeg (with encoders), lame and afconvert on this machine. Python 3 stdlib only.
Removes stale files under tests/fixtures/ first. Tests never need the encoders; only this
script does.
"""
import json, math, os, shutil, struct, subprocess, sys, tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FIXTURES = os.path.join(REPO, "tests", "fixtures")

# Signal: sum of three chirps over SWEEP_SECONDS, per-channel phase offsets.
SWEEP_SECONDS = 2.0
AMPLITUDES = [0.25, 0.20, 0.15]
# Phase added to each chirp, per channel. Right = cos(p1), -sin(p2 + 1.0), cos(p3 + 0.5).
PHASES = {
    "left": [0.0, 0.0, 0.0],
    "right": [math.pi / 2, 1.0 + math.pi, 0.5 + math.pi / 2],
}
SIGNAL_DESCRIPTION = (
    "stereo int16; sum of three chirps over %.1f s: log 40->2000 Hz, log 3000->250 Hz, "
    "lin 700->1300 Hz; sample n at rate r is evaluated at t=n/r; "
    "channel c = sum_i amplitudes[i]*sin(phase_i(t)+phases[c][i]); quantised round(x*32768)"
    % SWEEP_SECONDS
)

LENGTHS = {48000: [24011, 26003, 25013, 24989], 44100: [22063, 23887, 22979, 22961]}
FORMAT_SECONDS = 0.25
FORMAT_RATE = 48000

NO_LOCAL_ENCODER = ["amr", "awb", "ape", "dff", "dsf", "mpc", "shn", "spx", "tak"]

LAME = "libmp3lame"
# extension -> (codec name for the manifest, ffmpeg output args)
FORMATS = [
    ("aac", "aac", ["-c:a", "aac", "-f", "adts"]),
    ("ac3", "ac3", ["-c:a", "ac3"]),
    ("aif", "pcm_s16be", ["-c:a", "pcm_s16be", "-f", "aiff"]),
    ("aiff", "pcm_s16be", ["-c:a", "pcm_s16be"]),
    ("aifc", "pcm_s16be", ["-c:a", "pcm_s16be", "-f", "aiff"]),
    ("alac", "alac", ["-c:a", "alac", "-f", "ipod"]),
    ("au", "pcm_s16be", ["-c:a", "pcm_s16be"]),
    ("caf", "pcm_s16le", ["-c:a", "pcm_s16le"]),
    ("dts", "dca", ["-strict", "-2", "-c:a", "dca"]),
    ("flac", "flac", ["-c:a", "flac"]),
    ("m4a", "aac", ["-c:a", "aac"]),
    ("m4b", "aac", ["-c:a", "aac"]),
    ("mka", "flac", ["-c:a", "flac"]),
    ("mov", "aac", ["-c:a", "aac"]),
    ("mp2", "mp2", ["-c:a", "mp2"]),
    ("mp3", LAME, ["-c:a", LAME]),
    ("mp4", "aac", ["-c:a", "aac"]),
    ("oga", "flac", ["-c:a", "flac"]),
    ("ogg", "vorbis", ["-c:a", "vorbis", "-strict", "-2"]),
    ("opus", "libopus", ["-c:a", "libopus"]),
    # real_144 only takes 8 kHz mono.
    ("ra", "real_144", ["-ar", "8000", "-ac", "1", "-c:a", "real_144"]),
    ("tta", "tta", ["-c:a", "tta"]),
    ("wav", "pcm_s16le", ["-c:a", "pcm_s16le"]),
    ("wave", "pcm_s16le", ["-c:a", "pcm_s16le", "-f", "wav"]),
    ("webm", "libopus", ["-c:a", "libopus"]),
    ("wma", "wmav2", ["-c:a", "wmav2"]),
    ("wv", "wavpack", ["-c:a", "wavpack"]),
]

# set name -> (rate, lossless, extension, encoder label, command template)
# Templates use {i} input wav, {o} output; "ffmpeg" means FF + args.
FF_BASE = ["-hide_banner", "-loglevel", "error", "-y"]
GAPLESS_SETS = [
    ("wav48", 48000, True, "wav", "ffmpeg pcm_s16le", "ffmpeg", ["-c:a", "pcm_s16le"]),
    ("flac48", 48000, True, "flac", "ffmpeg flac", "ffmpeg", ["-c:a", "flac"]),
    ("alac48", 48000, True, "m4a", "ffmpeg alac", "ffmpeg", ["-c:a", "alac"]),
    ("mp3cbr48", 48000, False, "mp3", "ffmpeg libmp3lame 192k CBR", "ffmpeg", ["-c:a", LAME, "-b:a", "192k"]),
    ("mp3vbr48", 48000, False, "mp3", "ffmpeg libmp3lame -q:a 2", "ffmpeg", ["-c:a", LAME, "-q:a", "2"]),
    ("mp3lame48", 48000, False, "mp3", "lame CLI 192k CBR", "lame", None),
    ("aac48", 48000, False, "m4a", "ffmpeg aac 192k", "ffmpeg", ["-c:a", "aac", "-b:a", "192k"]),
    ("aacapple48", 48000, False, "m4a", "afconvert -f m4af -d aac -b 192000", "afconvert", None),
    ("opus48", 48000, False, "opus", "ffmpeg libopus 160k", "ffmpeg", ["-c:a", "libopus", "-b:a", "160k"]),
    ("mp3notag48", 48000, False, "mp3", "ffmpeg libmp3lame 192k -write_xing 0", "ffmpeg",
     ["-c:a", LAME, "-b:a", "192k", "-write_xing", "0"]),
    ("wav44", 44100, True, "wav", "ffmpeg pcm_s16le", "ffmpeg", ["-c:a", "pcm_s16le"]),
    ("flac44", 44100, True, "flac", "ffmpeg flac", "ffmpeg", ["-c:a", "flac"]),
    ("mp3lame44", 44100, False, "mp3", "lame CLI 192k CBR", "lame", None),
    ("aacapple44", 44100, False, "m4a", "afconvert -f m4af -d aac -b 192000", "afconvert", None),
]


def die(msg):
    sys.exit("make_test_fixtures: " + msg)


def run(cmd):
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode:
        raise RuntimeError("%s failed: %s" % (" ".join(cmd), r.stderr.strip()[-400:]))
    return r.stdout


def first_line(cmd):
    r = subprocess.run(cmd, capture_output=True, text=True)
    return (r.stdout or r.stderr).strip().splitlines()[0]


def require_tools():
    ff, lame, af = shutil.which("ffmpeg"), shutil.which("lame"), "/usr/bin/afconvert"
    for name, path in (("ffmpeg", ff), ("ffprobe", shutil.which("ffprobe")), ("lame", lame)):
        if not path:
            die("%s not found on PATH" % name)
    if not os.access(af, os.X_OK):
        die("/usr/bin/afconvert not found (macOS only)")
    return ff, lame, af


def sample_block(rate, start, count):
    """Interleaved stereo int16 bytes for samples start..start+count at `rate`."""
    t0, t1 = 0.0, SWEEP_SECONDS
    k_log = lambda f0, f1: math.log(f1 / f0) / SWEEP_SECONDS
    ka, kb = k_log(40, 2000), k_log(3000, 250)
    out = bytearray()
    for n in range(start, start + count):
        t = n / rate
        p1 = 2 * math.pi * 40 * (math.exp(ka * t) - 1) / ka
        p2 = 2 * math.pi * 3000 * (math.exp(kb * t) - 1) / kb
        p3 = 2 * math.pi * (700 * t + (1300 - 700) * t * t / (2 * SWEEP_SECONDS))
        ps = (p1, p2, p3)
        for ch in ("left", "right"):
            x = sum(a * math.sin(p + ph) for a, p, ph in zip(AMPLITUDES, ps, PHASES[ch]))
            v = max(-32768, min(32767, int(math.floor(x * 32768.0 + 0.5))))
            out += struct.pack("<h", v)
    return bytes(out)


def write_wav(path, pcm, rate):
    with open(path, "wb") as f:
        f.write(b"RIFF" + struct.pack("<I", 36 + len(pcm)) + b"WAVEfmt ")
        f.write(struct.pack("<IHHIIHH", 16, 1, 2, rate, rate * 4, 4, 16))
        f.write(b"data" + struct.pack("<I", len(pcm)) + pcm)


def ffmpeg(ff, src, dst, args):
    run([ff, *FF_BASE, "-fflags", "+bitexact", "-flags:a", "+bitexact", "-i", src,
         "-map_metadata", "-1", "-bitexact", *args, dst])


def probe(path):
    out = run(["ffprobe", "-v", "error", "-select_streams", "a:0",
               "-show_entries", "stream=codec_name,sample_rate,channels", "-of", "json", path])
    return json.loads(out)["streams"][0]


def main():
    ff, lame, af = require_tools()
    fixtures = os.path.realpath(FIXTURES)
    repo = os.path.realpath(REPO)
    if os.path.commonpath([fixtures, repo]) != repo or fixtures == repo:
        die("tests/fixtures resolves outside the repo: %s" % fixtures)
    if os.path.exists(fixtures):
        shutil.rmtree(fixtures)
    os.makedirs(os.path.join(fixtures, "formats"))

    manifest = {
        "encoders": {"ffmpeg": first_line([ff, "-version"]), "lame": first_line([lame, "--version"]),
                     "afconvert": af},
        "signal": {"description": SIGNAL_DESCRIPTION, "amplitudes": AMPLITUDES,
                   "phases": [PHASES["left"], PHASES["right"]]},
        "formats": {"no_local_encoder": NO_LOCAL_ENCODER, "files": []},
        "gapless": {"sets": []},
    }

    with tempfile.TemporaryDirectory() as tmp:
        # Formats.
        fsrc = os.path.join(tmp, "formats.wav")
        write_wav(fsrc, sample_block(FORMAT_RATE, 0, int(FORMAT_RATE * FORMAT_SECONDS)), FORMAT_RATE)
        missing = []
        for ext, codec, args in FORMATS:
            rel = "formats/tone.%s" % ext
            dst = os.path.join(fixtures, rel)
            try:
                ffmpeg(ff, fsrc, dst, args)
                info = probe(dst)
                if not os.path.getsize(dst):
                    raise RuntimeError("empty output")
            except (RuntimeError, KeyError, IndexError) as e:
                missing.append("%s (%s)" % (ext, e))
                continue
            manifest["formats"]["files"].append({
                "extension": ext, "file": rel, "codec": codec,
                "rate": int(info["sample_rate"]), "channels": info["channels"],
                "seconds": FORMAT_SECONDS})
        if missing:
            die("cannot produce extensions: " + "; ".join(missing))

        # Gapless sources, one continuous rendering per rate.
        srcs = {}
        for rate, lengths in LENGTHS.items():
            pcm = sample_block(rate, 0, sum(lengths))
            parts, pos = [], 0
            for i, n in enumerate(lengths):
                p = os.path.join(tmp, "src%d_t%d.wav" % (rate, i + 1))
                write_wav(p, pcm[pos * 4:(pos + n) * 4], rate)
                parts.append(p)
                pos += n
            srcs[rate] = parts

        for name, rate, lossless, ext, label, kind, args in GAPLESS_SETS:
            files = []
            for i, src in enumerate(srcs[rate]):
                rel = "gapless/%s/t%d.%s" % (name, i + 1, ext)
                dst = os.path.join(fixtures, rel)
                os.makedirs(os.path.dirname(dst), exist_ok=True)
                try:
                    if kind == "ffmpeg":
                        ffmpeg(ff, src, dst, args)
                    elif kind == "lame":
                        run([lame, "--quiet", "-b", "192", "--cbr", src, dst])
                    else:
                        run([af, "-f", "m4af", "-d", "aac", "-b", "192000", src, dst])
                except RuntimeError as e:
                    die("set %s: %s" % (name, e))
                files.append(rel)
            manifest["gapless"]["sets"].append({
                "name": name, "rate": rate, "lossless": lossless, "files": files,
                "lengths": LENGTHS[rate], "encoder": label})

    with open(os.path.join(fixtures, "manifest.json"), "w") as f:
        json.dump(manifest, f, indent=2)
        f.write("\n")
    print("wrote", fixtures)


if __name__ == "__main__":
    main()
