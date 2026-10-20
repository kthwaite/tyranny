#!/usr/bin/env python3
"""Regenerate tests/fixtures/ (format and gapless test fixtures + manifest.json).

Usage: python3 scripts/make_test_fixtures.py

Needs ffmpeg (with encoders), lame and afconvert on this machine, plus the Homebrew formulas
speex, musepack, mac, opencore-amr and vo-amrwbenc. Python 3 stdlib only; the AMR encoder
libraries are called through ctypes. Shorten, DSF and DSDIFF have no encoder to install, so
this script writes them itself. Removes stale files under tests/fixtures/ first. Tests never
need the encoders; only this script does.
afconvert output (the 8 aacapple* files) is not byte-stable across runs; decoded lengths and
the manifest are, so regeneration dirtying those files is expected.
"""
import ctypes, json, math, os, re, shutil, struct, subprocess, sys, tempfile

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

# Scanner extensions with no fixture, and why. tests/ffmpeg_formats.rs requires every
# scanner extension to have a fixture or an entry here.
EXCEPTIONS = [
    {"extension": "tak",
     "reason": "the only TAK encoders are Windows programs, and there is no redistributable sample file"},
]

# Encoders outside ffmpeg -> the Homebrew formula that installs them.
BREW = {"speexenc": "speex", "mpcenc": "musepack", "mac": "mac",
        "libopencore-amrnb": "opencore-amr", "libvo-amrwbenc": "vo-amrwbenc"}
DSD_RATE = 2822400  # DSD64
# 1 bit per sample is 176 KB for 0.25 s of stereo; a sixteenth of a second is one WAV's size
# and still ends in a partial DSF block.
DSD_SECONDS = 0.0625

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
    # Opus has no fixed frame size, so the mov demuxer sends no end trim; only the
    # short final stts duration ends these tracks at their source length.
    ("mp4opus48", 48000, False, "mp4", "ffmpeg libopus 64k in mp4", "ffmpeg", ["-c:a", "libopus", "-b:a", "64k"]),
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
    text = re.sub(r"\x1b\[[0-9;]*m|\r", "", r.stdout or r.stderr)  # mpcenc colours its banner
    return text.strip().splitlines()[0]


def require_tools():
    ff, lame, af = shutil.which("ffmpeg"), shutil.which("lame"), "/usr/bin/afconvert"
    for name, path in (("ffmpeg", ff), ("ffprobe", shutil.which("ffprobe")), ("lame", lame)):
        if not path:
            die("%s not found on PATH" % name)
    if not os.access(af, os.X_OK):
        die("/usr/bin/afconvert not found (macOS only)")
    return ff, lame, af


def require_external():
    """Paths of the encoders outside ffmpeg, and the AMR encoder libraries, loaded."""
    tools = {}
    for name in ("speexenc", "mpcenc", "mac"):
        tools[name] = shutil.which(name)
        if not tools[name]:
            die("%s not found on PATH (brew install %s)" % (name, BREW[name]))
    prefixes = dict.fromkeys(p for p in (os.environ.get("HOMEBREW_PREFIX"), "/opt/homebrew", "/usr/local") if p)
    for name in ("libopencore-amrnb", "libvo-amrwbenc"):
        paths = [os.path.join(p, "lib", name + ".dylib") for p in prefixes]
        found = next((p for p in paths if os.path.exists(p)), None)
        if not found:
            die("%s not found at %s (brew install %s)" % (name, " or ".join(paths), BREW[name]))
        tools[name] = ctypes.CDLL(found)
        tools[name + " path"] = os.path.realpath(found)
    return tools


def signal(t):
    """(left, right) signal values at time t."""
    k_log = lambda f0, f1: math.log(f1 / f0) / SWEEP_SECONDS
    ka, kb = k_log(40, 2000), k_log(3000, 250)
    p1 = 2 * math.pi * 40 * (math.exp(ka * t) - 1) / ka
    p2 = 2 * math.pi * 3000 * (math.exp(kb * t) - 1) / kb
    p3 = 2 * math.pi * (700 * t + (1300 - 700) * t * t / (2 * SWEEP_SECONDS))
    ps = (p1, p2, p3)
    return tuple(sum(a * math.sin(p + ph) for a, p, ph in zip(AMPLITUDES, ps, PHASES[ch]))
                 for ch in ("left", "right"))


def sample_block(rate, start, count):
    """Interleaved stereo int16 bytes for samples start..start+count at `rate`."""
    out = bytearray()
    for n in range(start, start + count):
        for x in signal(n / rate):
            v = max(-32768, min(32767, int(math.floor(x * 32768.0 + 0.5))))
            out += struct.pack("<h", v)
    return bytes(out)


def split_channels(pcm):
    """Interleaved stereo int16 bytes -> ([left samples], [right samples])."""
    v = struct.unpack("<%dh" % (len(pcm) // 2), pcm)
    return list(v[0::2]), list(v[1::2])


def write_wav(path, pcm, rate):
    with open(path, "wb") as f:
        f.write(b"RIFF" + struct.pack("<I", 36 + len(pcm)) + b"WAVEfmt ")
        f.write(struct.pack("<IHHIIHH", 16, 1, 2, rate, rate * 4, 4, 16))
        f.write(b"data" + struct.pack("<I", len(pcm)) + pcm)


def ffmpeg(ff, src, dst, args):
    run([ff, *FF_BASE, "-fflags", "+bitexact", "-flags:a", "+bitexact", "-i", src,
         "-map_metadata", "-1", "-bitexact", *args, dst])


def cli_frames(ff, path):
    """Stereo frames the ffmpeg CLI decodes at the engine rate: the reference count."""
    r = subprocess.run([ff, "-v", "error", "-i", path, "-f", "f32le", "-ac", "2", "-ar", "48000", "-"],
                       capture_output=True)
    if r.returncode:
        raise RuntimeError("decoding %s failed: %s" % (path, r.stderr.decode(errors="replace")[-400:]))
    return len(r.stdout) // 8


def packet_frames(path):
    """Frames at the engine rate that the packet durations add up to. For Ogg Speex the
    demuxer shortens the final packet to the end granule; the ffmpeg CLI ignores that and
    decodes the whole packet, but Decoder cuts it (src/decoder.rs), so this is the reference."""
    out = run(["ffprobe", "-v", "error", "-select_streams", "a:0", "-show_entries",
               "stream=time_base:packet=duration", "-of", "json", path])
    info = json.loads(out)
    num, den = map(int, info["streams"][0]["time_base"].split("/"))
    total = sum(int(p["duration"]) for p in info["packets"]) * 48000 * num
    if total % den:
        raise RuntimeError("%s: packet durations are not whole frames at 48 kHz" % path)
    return total // den


def probe(path):
    out = run(["ffprobe", "-v", "error", "-select_streams", "a:0",
               "-show_entries", "stream=codec_name,sample_rate,channels", "-of", "json", path])
    return json.loads(out)["streams"][0]


def check_lossless(ff, path, pcm):
    """The ffmpeg CLI must decode `path` to exactly the interleaved int16 `pcm`."""
    r = subprocess.run([ff, "-v", "error", "-i", path, "-f", "s16le", "-"], capture_output=True)
    if r.returncode or r.stdout != pcm:
        raise RuntimeError("%s does not decode to its source PCM" % path)


def ogg_crc(page):
    crc = 0
    for b in page:
        crc ^= b << 24
        for _ in range(8):
            crc = ((crc << 1) ^ 0x04C11DB7 if crc & 0x80000000 else crc << 1) & 0xFFFFFFFF
    return crc


def pin_ogg_serial(data, serial=0x54595241):
    """speexenc picks a random Ogg stream serial; pin it so the file is byte-stable."""
    out, pos = bytearray(), 0
    while pos < len(data):
        if data[pos:pos + 4] != b"OggS":
            raise RuntimeError("no Ogg page at byte %d" % pos)
        segments = data[pos + 26]
        size = 27 + segments + sum(data[pos + 27:pos + 27 + segments])
        page = bytearray(data[pos:pos + size])
        page[14:18] = struct.pack("<I", serial)
        page[22:26] = bytes(4)
        page[22:26] = struct.pack("<I", ogg_crc(page))
        out += page
        pos += size
    return bytes(out)


def encode_amr(lib, samples, wideband):
    """AMR file (RFC 4867 storage format) of mono int16 samples, zero-padded to whole frames.
    Narrowband: opencore-amr at 12.2 kbit/s, 160 samples at 8 kHz per frame. Wideband:
    vo-amrwbenc at 23.85 kbit/s, 320 samples at 16 kHz per frame."""
    if wideband:
        magic, size, mode = b"#!AMR-WB\n", 320, 8
        init, encode, close = lib.E_IF_init, lib.E_IF_encode, lib.E_IF_exit
        init.argtypes = []
    else:
        magic, size, mode = b"#!AMR\n", 160, 7
        init, encode, close = lib.Encoder_Interface_init, lib.Encoder_Interface_Encode, lib.Encoder_Interface_exit
        init.argtypes = [ctypes.c_int]
    init.restype = ctypes.c_void_p
    encode.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.POINTER(ctypes.c_short),
                       ctypes.POINTER(ctypes.c_ubyte), ctypes.c_int]
    close.argtypes = [ctypes.c_void_p]
    state = init() if wideband else init(0)
    if not state:
        raise RuntimeError("AMR encoder init failed")
    out = bytearray(magic)
    samples = samples + [0] * (-len(samples) % size)
    try:
        for i in range(0, len(samples), size):
            buf = (ctypes.c_ubyte * 64)()
            n = encode(state, mode, (ctypes.c_short * size)(*samples[i:i + size]), buf, 0)
            if n <= 0:
                raise RuntimeError("AMR encode returned %d" % n)
            out += bytes(buf[:n])
    finally:
        close(state)
    return bytes(out)


class BitWriter:
    """MSB-first bit stream."""

    def __init__(self):
        self.out, self.acc, self.n = bytearray(), 0, 0

    def bits(self, value, n):
        self.acc, self.n = (self.acc << n) | value, self.n + n
        while self.n >= 8:
            self.n -= 8
            self.out.append((self.acc >> self.n) & 0xFF)
        self.acc &= (1 << self.n) - 1

    def uvar(self, value, k):
        """Shorten's unsigned Rice code: value >> k in unary (zeros, then a one), then k low bits."""
        self.bits(1, (value >> k) + 1)
        self.bits(value & ((1 << k) - 1), k)

    def uint(self, value):
        """Shorten version >= 1 ulong: the Rice parameter first, then the value."""
        k = value.bit_length()
        self.uvar(k, 2)
        self.uvar(value, k)

    def finish(self):
        if self.n:
            self.bits(0, 8 - self.n)
        return bytes(self.out)


def rice_cost(u, k):
    return sum((v >> k) + 1 + k for v in u)


def c_div(a, b):
    """C integer division (truncates toward zero)."""
    q = abs(a) // abs(b)
    return q if (a < 0) == (b < 0) else -q


def encode_shorten(header, chans, blocksize=256, nmean=4):
    """Shorten version 2 stream of int16 little-endian channels, as libavcodec/shorten.c reads
    it: the WAV header in FN_VERBATIM, then per block and channel the cheapest fixed
    predictor (FN_DIFF0..FN_DIFF3) with a Rice-coded residual, FN_BLOCKSIZE for a short last
    block, and FN_QUIT."""
    w = BitWriter()
    for b in b"ajkg\x02":
        w.bits(b, 8)
    for v in (5, len(chans), blocksize, 0, nmean, 0):  # TYPE_S16LH, channels, blocksize, maxnlpc, nmean, skip
        w.uint(v)
    w.uvar(9, 2)  # FN_VERBATIM
    w.uvar(len(header), 5)
    for b in header:
        w.uvar(b, 8)
    history = [[0, 0, 0] for _ in chans]  # the decoder's nwrap = 3 wrap samples
    means = [[0] * nmean for _ in chans]
    current = blocksize
    for start in range(0, len(chans[0]), blocksize):
        size = min(blocksize, len(chans[0]) - start)
        if size != current:
            w.uvar(5, 2)  # FN_BLOCKSIZE
            w.uint(size)
            current = size
        for c, samples in enumerate(chans):
            x = samples[start:start + size]
            e = history[c] + x  # e[i + 3] is x[i]
            coffset = c_div(nmean // 2 + sum(means[c]), nmean)
            residuals = [
                [v - coffset for v in x],
                [e[i + 3] - e[i + 2] for i in range(size)],
                [e[i + 3] - 2 * e[i + 2] + e[i + 1] for i in range(size)],
                [e[i + 3] - 3 * e[i + 2] + 3 * e[i + 1] - e[i] for i in range(size)],
            ]
            best = None
            for cmd, r in enumerate(residuals):
                u = [2 * v if v >= 0 else -2 * v - 1 for v in r]
                for k in range(31):  # the signed code uses k + 1 low bits
                    cost = rice_cost(u, k + 1)
                    if best is None or cost < best[0]:
                        best = (cost, cmd, k, u)
            _, cmd, k, u = best
            w.uvar(cmd, 2)
            w.uvar(k, 3)
            for v in u:
                w.uvar(v, k + 1)
            means[c] = means[c][1:] + [c_div(size // 2 + sum(x), size)]
            history[c] = e[-3:]
    w.uvar(4, 2)  # FN_QUIT
    return w.finish()


def dsd_bits(seconds):
    """Per-channel DSD64 bits (1 = +1) of the signal at half scale, from a second-order
    error-feedback sigma-delta modulator (noise transfer function (1 - z^-1)^2)."""
    out, errors = ([], []), [[0.0, 0.0], [0.0, 0.0]]
    for i in range(int(DSD_RATE * seconds)):
        for c, x in enumerate(signal(i / DSD_RATE)):
            e1, e2 = errors[c]
            u = 0.5 * x - 2 * e1 + e2
            y = 1.0 if u >= 0 else -1.0
            errors[c] = [y - u, e1]
            out[c].append(y > 0)
    return out


def pack_bits(bits, lsb_first):
    out = bytearray()
    for i in range(0, len(bits), 8):
        out.append(sum(b << (j if lsb_first else 7 - j) for j, b in enumerate(bits[i:i + 8])))
    return bytes(out)


def dsf_file(chans, block=4096):
    """Sony DSF: stereo, LSB first, 4096-byte blocks per channel, the last zero-padded.
    Returns (file bytes, the DSD payload the dsf demuxer should read back)."""
    data, payload = bytearray(), bytearray()
    for i in range(0, len(chans[0]), block):
        for ch in chans:
            data += ch[i:i + block].ljust(block, b"\0")
            payload += ch[i:i + block]
    # version 1, format 0 (DSD raw), channel type 2 (stereo), channels, rate, 1 bit (LSB
    # first), samples per channel, block size per channel, reserved.
    fmt = b"fmt " + struct.pack("<QIIIIIIQII", 52, 1, 0, 2, len(chans), DSD_RATE, 1,
                                len(chans[0]) * 8, block, 0)
    body = fmt + b"data" + struct.pack("<Q", 12 + len(data)) + data
    return b"DSD " + struct.pack("<QQQ", 28, 28 + len(body), 0) + body, bytes(payload)


def dff_file(chans):
    """DSDIFF 1.5: FRM8/DSD with FVER, PROP/SND (FS, CHNL, CMPR DSD) and an interleaved,
    MSB-first DSD chunk. Returns (file bytes, the DSD payload the iff demuxer should read back)."""
    def chunk(ck, body):
        return ck + struct.pack(">Q", len(body)) + body + b"\0" * (len(body) % 2)
    name = b"not compressed"
    pstring = bytes([len(name)]) + name + b"\0" * ((len(name) + 1) % 2)
    prop = (b"SND " + chunk(b"FS  ", struct.pack(">I", DSD_RATE))
            + chunk(b"CHNL", struct.pack(">H", len(chans)) + b"SLFTSRGT")
            + chunk(b"CMPR", b"DSD " + pstring))
    data = bytes(b for frame in zip(*chans) for b in frame)
    body = (b"DSD " + chunk(b"FVER", struct.pack(">I", 0x01050000)) + chunk(b"PROP", prop)
            + chunk(b"DSD ", data))
    return chunk(b"FRM8", body), data


def check_dsd(ff, path, payload, pcm):
    """The demuxer must read back exactly our DSD payload, and the decode must follow the
    source signal (swapped channels do not correlate). Returns the decode. A wrong bit
    order still correlates (it only jitters samples within a byte), so the caller compares
    the DSF and DSDIFF decodes of the same bits instead."""
    r = subprocess.run([ff, "-v", "error", "-i", path, "-map", "0:a", "-c:a", "copy", "-f", "data", "-"],
                       capture_output=True)
    if r.returncode or r.stdout != payload:
        raise RuntimeError("%s: the demuxer does not read back the written DSD data" % path)
    r = subprocess.run([ff, "-v", "error", "-i", path, "-f", "f32le", "-ac", "2", "-ar", str(FORMAT_RATE), "-"],
                       capture_output=True)
    if r.returncode:
        raise RuntimeError("decoding %s failed: %s" % (path, r.stderr.decode(errors="replace")[-400:]))
    got = struct.unpack("<%df" % (len(r.stdout) // 4), r.stdout)
    want = struct.unpack("<%dh" % (len(pcm) // 2), pcm)
    # The decoder's lowpass delays the output; take the best lag, then require both channels
    # to correlate with the source.
    def corr(c, lag):
        a, b = got[2 * lag + c::2], want[c::2]
        n = min(len(a), len(b)) - 64  # skip the filters' edges
        a, b = a[64:n], b[64:n]
        return sum(p * q for p, q in zip(a, b)) / math.sqrt(sum(p * p for p in a) * sum(q * q for q in b))
    lag = max(range(64), key=lambda lag: corr(0, lag))
    worst = min(corr(0, lag), corr(1, lag))
    if worst < 0.99:
        raise RuntimeError("%s decodes unlike its source (correlation %.4f at lag %d)" % (path, worst, lag))
    return r.stdout


def external_makers(ff, tools, tmp, pcm):
    """(extension, seconds, function(dst), reference frame counter) for the formats ffmpeg
    cannot encode. `pcm` is the FORMAT_RATE source, also in tmp/formats.wav."""
    src = os.path.join(tmp, "formats.wav")
    dsd, decodes = [], []  # both DSD containers carry the same bits; render them once

    def write(dst, data):
        with open(dst, "wb") as f:
            f.write(data)

    def spx(dst):
        # Speex has no 48 kHz mode; this is wideband, 16 kHz.
        wav = os.path.join(tmp, "formats16000.wav")
        write_wav(wav, sample_block(16000, 0, int(16000 * FORMAT_SECONDS)), 16000)
        run([tools["speexenc"], wav, dst])
        with open(dst, "rb") as f:
            write(dst, pin_ogg_serial(f.read()))

    def amr(dst, rate, lib, wideband):
        # AMR is mono at a fixed rate: the left channel at that rate.
        left, _ = split_channels(sample_block(rate, 0, int(rate * FORMAT_SECONDS)))
        write(dst, encode_amr(tools[lib], left, wideband))

    def shn(dst):
        with open(src, "rb") as f:
            write(dst, encode_shorten(f.read(44), split_channels(pcm)))
        check_lossless(ff, dst, pcm)

    def ape(dst):
        run([tools["mac"], src, dst, "-c2000"])
        check_lossless(ff, dst, pcm)

    def dsd_file(dst, container, lsb_first):
        if not dsd:
            dsd.extend(dsd_bits(DSD_SECONDS))
        data, payload = container([pack_bits(bits, lsb_first) for bits in dsd])
        write(dst, data)
        decodes.append(check_dsd(ff, dst, payload, pcm))
        if len(decodes) == 2 and decodes[0] != decodes[1]:
            raise RuntimeError("tone.dff and tone.dsf carry the same bits but decode differently: "
                               "one container's bit order is wrong")

    cli = lambda path: cli_frames(ff, path)
    return [
        ("amr", FORMAT_SECONDS, lambda dst: amr(dst, 8000, "libopencore-amrnb", False), cli),
        ("awb", FORMAT_SECONDS, lambda dst: amr(dst, 16000, "libvo-amrwbenc", True), cli),
        ("ape", FORMAT_SECONDS, ape, cli),
        ("dff", DSD_SECONDS, lambda dst: dsd_file(dst, dff_file, False), cli),
        ("dsf", DSD_SECONDS, lambda dst: dsd_file(dst, dsf_file, True), cli),
        ("mpc", FORMAT_SECONDS, lambda dst: run([tools["mpcenc"], "--silent", "--overwrite", src, dst]), cli),
        ("shn", FORMAT_SECONDS, shn, cli),
        ("spx", FORMAT_SECONDS, spx, packet_frames),
    ]


def stream_fixtures(ff, fixtures):
    """Four seconds of generated 440 Hz tone; no encoder is needed by tests."""
    directory = os.path.join(fixtures, "streams")
    os.makedirs(directory, exist_ok=True)
    signal = ["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=4"]
    run([ff, *FF_BASE, *signal, "-c:a", LAME, "-b:a", "64k",
         "-reservoir", "0", "-write_xing", "0", "-id3v2_version", "0",
         os.path.join(directory, "live.mp3")])
    run([ff, *FF_BASE, *signal, "-ac", "2", "-c:a", "vorbis", "-strict", "experimental",
         os.path.join(directory, "live.ogg")])


def main():
    ff, lame, af = require_tools()
    tools = require_external()
    fixtures = os.path.realpath(FIXTURES)
    repo = os.path.realpath(REPO)
    if os.path.commonpath([fixtures, repo]) != repo or fixtures == repo:
        die("tests/fixtures resolves outside the repo: %s" % fixtures)
    if os.path.exists(fixtures):
        shutil.rmtree(fixtures)
    os.makedirs(os.path.join(fixtures, "formats"))

    manifest = {
        "encoders": {"ffmpeg": first_line([ff, "-version"]), "lame": first_line([lame, "--version"]),
                     "afconvert": af, "speexenc": first_line([tools["speexenc"], "--version"]),
                     "mpcenc": first_line([tools["mpcenc"], "--version"]), "mac": first_line([tools["mac"]]),
                     "opencore-amr": tools["libopencore-amrnb path"],
                     "vo-amrwbenc": tools["libvo-amrwbenc path"],
                     "shn, dsf, dff": "written by scripts/make_test_fixtures.py"},
        "signal": {"description": SIGNAL_DESCRIPTION, "amplitudes": AMPLITUDES,
                   "phases": [PHASES["left"], PHASES["right"]]},
        "formats": {"exceptions": EXCEPTIONS, "files": []},
        "gapless": {"sets": []},
    }

    with tempfile.TemporaryDirectory() as tmp:
        # Formats.
        fsrc = os.path.join(tmp, "formats.wav")
        fpcm = sample_block(FORMAT_RATE, 0, int(FORMAT_RATE * FORMAT_SECONDS))
        write_wav(fsrc, fpcm, FORMAT_RATE)
        # (extension, manifest codec or None for ffprobe's name, seconds, function(dst),
        # reference frame counter)
        makers = [(ext, codec, FORMAT_SECONDS, lambda dst, args=args: ffmpeg(ff, fsrc, dst, args),
                   lambda path: cli_frames(ff, path)) for ext, codec, args in FORMATS]
        makers += [(ext, None, seconds, make, frames)
                   for ext, seconds, make, frames in external_makers(ff, tools, tmp, fpcm)]
        missing = []
        for ext, codec, seconds, make, count_frames in makers:
            rel = "formats/tone.%s" % ext
            dst = os.path.join(fixtures, rel)
            try:
                make(dst)
                info = probe(dst)
                if not os.path.getsize(dst):
                    raise RuntimeError("empty output")
                frames = count_frames(dst)
            except (RuntimeError, KeyError, IndexError) as e:
                missing.append("%s (%s)" % (ext, e))
                continue
            entry = {"extension": ext, "file": rel, "codec": codec or info["codec_name"],
                     "rate": int(info["sample_rate"]), "channels": info["channels"],
                     "seconds": seconds, "frames": frames}
            if count_frames is packet_frames:
                entry["frames_from"] = "packet durations"
            manifest["formats"]["files"].append(entry)
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

    stream_fixtures(ff, fixtures)

    with open(os.path.join(fixtures, "manifest.json"), "w") as f:
        json.dump(manifest, f, indent=2)
        f.write("\n")
    print("wrote", fixtures)


if __name__ == "__main__":
    main()
