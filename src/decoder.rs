//! One demuxer, decoder and resampler per source; no playback state is shared.
//!
//! libavcodec applies AV_PKT_DATA_SKIP_SAMPLES itself (SKIP_MANUAL is deliberately
//! not enabled). Its returned sample count and adjusted timestamps already exclude
//! that padding. Sample-exact frame durations additionally delimit container edits,
//! including the short final AAC/MP4 packet. Neither stream duration estimates nor
//! codec-specific delay constants are used to trim the decoded audio.

use std::{path::Path, ptr, sync::LazyLock};

use anyhow::{Context, Result, bail, ensure};
use ffmpeg::{ChannelLayout, Rational, ffi, format::Sample, packet::Mut};
use ffmpeg_next as ffmpeg;
use serde::{Deserialize, Serialize};

use crate::{
    BLOCK_FRAMES, Frame, SAMPLE_RATE,
    metadata::{PlaybackMetadata, playback_metadata},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GainMode {
    Off,
    Track,
    Album,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct GainMetadata {
    pub(crate) track_gain: Option<f64>,
    pub(crate) track_peak: Option<f64>,
    pub(crate) album_gain: Option<f64>,
    pub(crate) album_peak: Option<f64>,
}

impl GainMetadata {
    pub(crate) fn tag(&mut self, key: &str, value: &str) {
        let field = if key.eq_ignore_ascii_case("replaygain_track_gain") {
            &mut self.track_gain
        } else if key.eq_ignore_ascii_case("replaygain_album_gain") {
            &mut self.album_gain
        } else if key.eq_ignore_ascii_case("replaygain_track_peak") {
            &mut self.track_peak
        } else if key.eq_ignore_ascii_case("replaygain_album_peak") {
            &mut self.album_peak
        } else {
            return;
        };
        // Malformed/nonfinite metadata is absent, not a NaN audio multiplier.
        // A malformed stream override must not erase a valid container tag.
        if let Some(value) = tag_number(value) {
            *field = Some(value);
        }
    }

    pub(crate) fn side_data(&mut self, bytes: &[u8]) -> Result<()> {
        ensure!(
            bytes.len() >= std::mem::size_of::<ffi::AVReplayGain>(),
            "truncated FFmpeg ReplayGain side data"
        );
        // AVReplayGain is native-endian ABI data, not a serialized byte format.
        // Side-data alignment is not promised by the Rust slice's type.
        let gain = unsafe { ptr::read_unaligned(bytes.as_ptr().cast::<ffi::AVReplayGain>()) };
        self.track_gain = self.track_gain.or(raw_gain(gain.track_gain));
        self.album_gain = self.album_gain.or(raw_gain(gain.album_gain));
        self.track_peak = self.track_peak.or(raw_peak(gain.track_peak));
        self.album_peak = self.album_peak.or(raw_peak(gain.album_peak));
        Ok(())
    }

    fn selected_db(self, mode: GainMode) -> f64 {
        let (gain, peak) = match mode {
            GainMode::Off => return 0.0,
            GainMode::Track => (self.track_gain, self.track_peak),
            GainMode::Album if self.album_gain.is_some() => {
                (self.album_gain, self.album_peak.or(self.track_peak))
            }
            // Album gain AND its peak fall back together. An unrelated album
            // peak cannot limit the track fallback when album gain is absent.
            GainMode::Album => (self.track_gain, self.track_peak),
        };
        let requested = gain.unwrap_or(0.0);
        match peak.filter(|peak| peak.is_finite() && *peak > 0.0) {
            Some(peak) => requested.min(-20.0 * peak.log10()),
            None => requested,
        }
    }
}

fn tag_number(value: &str) -> Option<f64> {
    let value = value.trim();
    let number = match value.get(value.len().saturating_sub(2)..) {
        Some(suffix) if suffix.eq_ignore_ascii_case("db") => &value[..value.len() - 2],
        _ => value,
    };
    number.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

fn raw_gain(value: i32) -> Option<f64> {
    (value != i32::MIN).then_some(f64::from(value) / 100_000.0)
}

fn raw_peak(value: u32) -> Option<f64> {
    (value != 0).then_some(f64::from(value) / 100_000.0)
}

pub(crate) fn initialize_ffmpeg() -> Result<()> {
    static INIT: LazyLock<std::result::Result<(), ffmpeg::Error>> = LazyLock::new(ffmpeg::init);
    INIT.as_ref()
        .map_err(|error| anyhow::anyhow!("initializing FFmpeg: {error}"))
        .copied()
}

pub struct Decoder {
    input: ffmpeg::format::context::Input,
    codec: ffmpeg::decoder::Audio,
    packet: ffmpeg::Packet,
    decoded: ffmpeg::frame::Audio,
    resampler: Option<ffmpeg::software::resampling::Context>,
    stream_index: usize,
    time_base: Rational,
    origin_ts: Option<i64>,
    duration: Option<u64>,
    metadata: Option<PlaybackMetadata>,
    seek_preroll: u32,
    gain_db: f64,
    multiplier: f32,
    frame_offset: usize,
    frame_samples: usize,
    // Mono stays mono through swresample and is duplicated only on read. This
    // avoids the default -3 dB mono-to-stereo mixing matrix entirely.
    output_channels: usize,
    samples: Box<[f32; BLOCK_FRAMES * 2]>,
    buffered: usize,
    buffer_offset: usize,
    demux_eof: bool,
    decoder_eof: bool,
    drained: bool,
    next_output: Option<i64>,
    seek_target: Option<i64>,
    alignment_skip: usize,
}

impl Decoder {
    pub fn open(path: &Path, mode: GainMode) -> Result<Self> {
        initialize_ffmpeg()?;
        let mut input = ffmpeg::format::input(path)
            .with_context(|| format!("opening audio source {}", path.display()))?;
        // Match the codec's strict error policy at the demuxer, without asking
        // libavformat to discard corrupt packets or synthesize missing audio.
        unsafe {
            (*input.as_mut_ptr()).error_recognition =
                ffi::AV_EF_CRCCHECK | ffi::AV_EF_BITSTREAM | ffi::AV_EF_BUFFER | ffi::AV_EF_EXPLODE;
        }
        let stream = input
            .streams()
            .best(ffmpeg::media::Type::Audio)
            .context("source has no audio stream")?;
        let stream_index = stream.index();
        let time_base = stream.time_base();
        ensure!(
            time_base.0 > 0 && time_base.1 > 0,
            "invalid audio time base"
        );
        let origin_ts = (stream.start_time() != ffi::AV_NOPTS_VALUE).then_some(stream.start_time());
        let duration = (stream.duration() >= 0)
            .then(|| rescale(stream.duration(), time_base, output_time_base()))
            .filter(|duration| *duration >= 0)
            .map(|duration| duration as u64);
        let (display, mut metadata) = playback_metadata(&input, &stream, path);
        let parameters = stream.parameters();
        // seek_preroll is not exposed by ffmpeg-next's Parameters wrapper.
        let seek_preroll = unsafe { (*parameters.as_ptr()).seek_preroll.max(0) as u32 };
        let mut codec = ffmpeg::codec::context::Context::from_parameters(parameters)
            .context("copying audio codec parameters")?
            .decoder();
        codec.set_packet_time_base(time_base);
        codec.check(
            ffmpeg::decoder::Check::CRC
                | ffmpeg::decoder::Check::BITSTREAM
                | ffmpeg::decoder::Check::BUFFER
                | ffmpeg::decoder::Check::EXPLODE,
        );
        let codec = codec.audio().context("opening native audio decoder")?;
        let mut this = Self {
            input,
            codec,
            packet: ffmpeg::Packet::empty(),
            decoded: ffmpeg::frame::Audio::empty(),
            resampler: None,
            stream_index,
            time_base,
            origin_ts,
            duration,
            metadata: Some(display),
            seek_preroll,
            gain_db: 0.0,
            multiplier: 1.0,
            frame_offset: 0,
            frame_samples: 0,
            output_channels: 2,
            samples: Box::new([0.0; BLOCK_FRAMES * 2]),
            buffered: 0,
            buffer_offset: 0,
            demux_eof: false,
            decoder_eof: false,
            drained: false,
            next_output: Some(0),
            seek_target: None,
            alignment_skip: 0,
        };
        // Prime one decoded frame, not the whole track. This also surfaces
        // malformed sources during preparation and gets codec/frame gain data
        // before any samples can escape. Gain remains fixed for this source.
        if this.receive_frame()?
            && let Some(side) = this
                .decoded
                .side_data(ffmpeg::frame::side_data::Type::ReplayGain)
            && let Err(error) = metadata.side_data(side.data())
        {
            this.metadata
                .as_mut()
                .unwrap()
                .metadata_error
                .get_or_insert_with(|| format!("{error:#}"));
        }
        this.gain_db = metadata.selected_db(mode);
        this.multiplier = 10.0_f64.powf(this.gain_db / 20.0) as f32;
        ensure!(
            this.multiplier.is_finite(),
            "ReplayGain multiplier exceeds PCM range"
        );
        Ok(this)
    }

    pub(crate) fn take_metadata(&mut self) -> PlaybackMetadata {
        self.metadata.take().expect("source metadata already taken")
    }

    /// Fill a nonempty destination; zero means both codec and resampler drained.
    pub fn read(&mut self, destination: &mut [Frame]) -> Result<usize> {
        ensure!(
            !destination.is_empty(),
            "decoder destination must be nonempty"
        );
        let mut written = 0;
        while written < destination.len() {
            if self.buffer_offset == self.buffered && !self.refill()? {
                break;
            }
            let count = (self.buffered - self.buffer_offset).min(destination.len() - written);
            for output in &mut destination[written..written + count] {
                let offset = self.buffer_offset * self.output_channels;
                let left = self.samples[offset];
                let right = self.samples[offset + self.output_channels - 1];
                let frame = if self.multiplier == 1.0 {
                    [left, right]
                } else {
                    [left * self.multiplier, right * self.multiplier]
                };
                ensure!(
                    frame[0].is_finite() && frame[1].is_finite(),
                    "nonfinite decoded PCM"
                );
                *output = frame;
                self.buffer_offset += 1;
            }
            written += count;
        }
        Ok(written)
    }

    pub fn seek(&mut self, output_frame: u64) -> Result<()> {
        let target = i64::try_from(output_frame).context("seek target exceeds timestamp range")?;
        let origin = self
            .origin_ts
            .context("audio has no timestamp origin for seeking")?;
        let rate = self.codec.rate();
        ensure!(
            rate > 0 && rate <= i32::MAX as u32,
            "invalid audio rate for seeking"
        );
        // A bounded quarter-second warm-up covers reservoir/filter history even
        // when a codec does not declare it; declared larger preroll wins. This
        // is seek-only history, never encoder-delay compensation or a rewind to
        // decode the entire file. Everything discarded is before the target.
        let preroll = self
            .seek_preroll
            .max(self.codec.delay() as u32)
            .max(rate / 4);
        let preroll_frames = rescale(
            i64::from(preroll),
            Rational(1, rate as i32),
            output_time_base(),
        );
        let before = target.saturating_sub(preroll_frames).max(0);
        let timestamp = origin
            .checked_add(rescale(before, output_time_base(), self.time_base))
            .context("seek timestamp overflow")?;
        // Input::seek always uses stream=-1 / AV_TIME_BASE. Specify the selected
        // audio stream to retain its exact time base and seek index instead.
        let result = unsafe {
            ffi::avformat_seek_file(
                self.input.as_mut_ptr(),
                self.stream_index as i32,
                i64::MIN,
                timestamp,
                timestamp,
                0,
            )
        };
        if result < 0 {
            return Err(ffmpeg::Error::from(result)).context("seeking audio source");
        }
        self.codec.flush();
        unsafe {
            ffi::av_packet_unref(self.packet.as_mut_ptr());
            ffi::av_frame_unref(self.decoded.as_mut_ptr());
        }
        self.resampler = None;
        self.frame_offset = 0;
        self.frame_samples = 0;
        self.buffer_offset = 0;
        self.buffered = 0;
        self.demux_eof = false;
        self.decoder_eof = false;
        self.drained = false;
        self.next_output = None;
        self.seek_target = Some(target);
        self.alignment_skip = 0;
        // Actually decode/discard preroll now so seek errors are not deferred
        // until after a successful transport acknowledgement.
        self.refill()?;
        Ok(())
    }

    pub fn gain_db(&self) -> f64 {
        self.gain_db
    }

    /// Container duration until EOF, then the actual drained PCM extent.
    /// This is informational, never a sample-trimming or early-EOF boundary.
    pub fn duration_frames(&self) -> Option<u64> {
        self.duration
    }

    fn receive_frame(&mut self) -> Result<bool> {
        if self.decoder_eof {
            return Ok(false);
        }
        loop {
            match self.codec.receive_frame(&mut self.decoded) {
                Ok(()) => {
                    ensure!(
                        !self.decoded.is_corrupt() && !self.decoded.has_decode_errors(),
                        "decoder returned corrupt audio"
                    );
                    if self.decoded.samples() == 0 {
                        continue;
                    }
                    self.prepare_frame()?;
                    return Ok(true);
                }
                Err(ffmpeg::Error::Eof) => {
                    self.decoder_eof = true;
                    return Ok(false);
                }
                Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => {
                    ensure!(
                        !self.demux_eof,
                        "decoder requested input after drain was started"
                    );
                }
                Err(error) => return Err(error).context("receiving decoded audio"),
            }
            loop {
                // Unlike Input::packets(), direct Packet::read preserves every
                // demux error instead of silently retrying errors other than EOF.
                unsafe { ffi::av_packet_unref(self.packet.as_mut_ptr()) };
                match self.packet.read(&mut self.input) {
                    Ok(()) if self.packet.stream() != self.stream_index => continue,
                    Ok(()) => {
                        ensure!(
                            !self.packet.is_corrupt(),
                            "demuxer returned a corrupt audio packet"
                        );
                        self.codec
                            .send_packet(&self.packet)
                            .context("submitting audio packet")?;
                    }
                    Err(ffmpeg::Error::Eof) => {
                        // A truncated/erroring AVIO can otherwise look like a
                        // clean demux EOF. EOF itself is not an I/O error here.
                        let io_error = unsafe {
                            let io = (*self.input.as_ptr()).pb;
                            if io.is_null() { 0 } else { (*io).error }
                        };
                        if io_error < 0 && ffmpeg::Error::from(io_error) != ffmpeg::Error::Eof {
                            return Err(ffmpeg::Error::from(io_error))
                                .context("reading audio source");
                        }
                        self.codec
                            .send_eof()
                            .context("starting audio decoder drain")?;
                        self.demux_eof = true;
                    }
                    Err(error) => return Err(error).context("demuxing audio packet"),
                }
                break;
            }
        }
    }

    fn prepare_frame(&mut self) -> Result<()> {
        let rate = self.decoded.rate();
        ensure!(
            rate > 0 && rate <= i32::MAX as u32,
            "invalid decoded sample rate"
        );
        let mut layout = self.decoded.channel_layout();
        ensure!(
            (1..=64).contains(&layout.channels()),
            "unsupported decoded channel count"
        );
        if layout.is_empty() {
            layout = ChannelLayout::default(layout.channels());
        }
        // ffmpeg-next's ChannelLayout is a shallow Copy. Do not retain a custom
        // map owned by a frame that receive_frame will unref on its next call.
        ensure!(
            layout.0.order == ffi::AVChannelOrder::AV_CHANNEL_ORDER_NATIVE,
            "custom or ambisonic channel maps are not supported"
        );
        let format = self.decoded.format();
        ensure!(format != Sample::None, "decoder returned no sample format");
        if let Some(resampler) = &self.resampler {
            let input = resampler.input();
            ensure!(
                input.rate == rate && input.format == format && input.channel_layout == layout,
                "audio parameters changed within a source"
            );
        } else {
            let output_layout = if layout.channels() == 1 {
                ChannelLayout::MONO
            } else {
                ChannelLayout::STEREO
            };
            self.output_channels = output_layout.channels() as usize;
            self.resampler = Some(
                ffmpeg::software::resampling::Context::get(
                    format,
                    layout,
                    rate,
                    Sample::F32(ffmpeg::format::sample::Type::Packed),
                    output_layout,
                    SAMPLE_RATE,
                )
                .context("initializing audio resampler")?,
            );
        }
        self.frame_samples = valid_samples(
            self.decoded.samples(),
            self.decoded.packet().duration,
            self.time_base,
            rate,
        );
        let timestamp = self.decoded.pts().or_else(|| self.decoded.timestamp());
        if self.origin_ts.is_none() {
            self.origin_ts = timestamp;
        }
        if self.next_output.is_none() {
            let timestamp = timestamp.context("decoded frame has no timestamp after seek")?;
            let origin = self.origin_ts.context("missing audio timeline origin")?;
            let delta = timestamp
                .checked_sub(origin)
                .context("audio timestamp overflow")?;
            let source_position = rescale(delta, self.time_base, Rational(1, rate as i32));
            // Restart the resampler on the same rational sample grid as a full
            // decode. At 44.1 -> 48 kHz this is every 147 source samples. Any
            // fractional-grid prefix removed here is strictly seek preroll.
            self.alignment_skip = grid_skip(source_position, rate);
            let aligned = source_position
                .checked_add(self.alignment_skip as i64)
                .context("resampler timestamp overflow")?;
            self.next_output = Some(rescale(
                aligned,
                Rational(1, rate as i32),
                output_time_base(),
            ));
        }
        self.frame_offset = self.alignment_skip.min(self.frame_samples);
        self.alignment_skip -= self.frame_offset;
        Ok(())
    }

    fn refill(&mut self) -> Result<bool> {
        self.buffer_offset = 0;
        self.buffered = 0;
        if self.drained {
            return Ok(false);
        }
        loop {
            let count = if self.frame_offset < self.frame_samples {
                self.convert_frame()?
            } else if self.receive_frame()? {
                continue;
            } else if let Some(resampler) = &mut self.resampler {
                let output = self.samples.as_mut_ptr().cast::<u8>();
                // The null input is used ONLY at final EOF, not to retrieve a
                // pending block midstream (which would flush the filter early).
                let result = unsafe {
                    ffi::swr_convert(
                        resampler.as_mut_ptr(),
                        &output,
                        BLOCK_FRAMES as i32,
                        ptr::null(),
                        0,
                    )
                };
                checked_samples(result)?
            } else {
                0
            };
            if count == 0 {
                if !self.decoder_eof {
                    continue;
                }
                let end = self.next_output.unwrap_or(0);
                if let Some(target) = self.seek_target {
                    ensure!(target <= end, "seek target is beyond decoded EOF");
                }
                self.duration = Some(end.max(0) as u64);
                self.drained = true;
                return Ok(false);
            }
            let start = self
                .next_output
                .context("missing resampled audio timeline")?;
            self.next_output = Some(
                start
                    .checked_add(count as i64)
                    .context("audio timeline overflow")?,
            );
            self.buffered = count;
            if let Some(target) = self.seek_target {
                ensure!(
                    start <= target,
                    "native seek landed after the requested sample"
                );
                self.buffer_offset = (target - start).min(count as i64) as usize;
                if self.buffer_offset == count {
                    continue;
                }
                self.seek_target = None;
            }
            return Ok(true);
        }
    }

    fn convert_frame(&mut self) -> Result<usize> {
        let rate = self.decoded.rate();
        // Feed bounded chunks even when a codec returns a very large frame. The
        // ratio bound also prevents unbounded swresample buffering on upsampling.
        let chunk = ((BLOCK_FRAMES as u64 * u64::from(rate)) / u64::from(SAMPLE_RATE))
            .clamp(1, BLOCK_FRAMES as u64) as usize;
        let count = (self.frame_samples - self.frame_offset).min(chunk);
        let channels = usize::from(self.decoded.channels());
        let planar = self.decoded.is_planar();
        let planes = if planar { channels } else { 1 };
        let stride = self.decoded.format().bytes() * if planar { 1 } else { channels };
        let mut inputs = [ptr::null::<u8>(); 64];
        // AVFrame owns/refcounts these buffers until the next receive_frame.
        // frame_offset/count are bounded by its (already trimmed) nb_samples.
        unsafe {
            let frame = &*self.decoded.as_ptr();
            ensure!(
                !frame.extended_data.is_null(),
                "decoded audio has no sample planes"
            );
            for (index, input) in inputs[..planes].iter_mut().enumerate() {
                let plane = *frame.extended_data.add(index);
                ensure!(!plane.is_null(), "decoded audio has a missing sample plane");
                *input = plane.add(self.frame_offset * stride);
            }
        }
        let output = self.samples.as_mut_ptr().cast::<u8>();
        let resampler = self.resampler.as_mut().context("missing audio resampler")?;
        // The wrapper's run/flush allocate AVFrames sized to the input, not to
        // bounded output capacity. swr_convert lets us reuse this fixed buffer.
        let result = unsafe {
            ffi::swr_convert(
                resampler.as_mut_ptr(),
                &output,
                BLOCK_FRAMES as i32,
                inputs.as_ptr(),
                count as i32,
            )
        };
        let produced = checked_samples(result)?;
        self.frame_offset += count;
        Ok(produced)
    }
}

fn checked_samples(result: i32) -> Result<usize> {
    if result < 0 {
        bail!("converting decoded audio: {}", ffmpeg::Error::from(result));
    }
    Ok(result as usize)
}

fn output_time_base() -> Rational {
    Rational(1, SAMPLE_RATE as i32)
}

fn rescale(value: i64, from: Rational, to: Rational) -> i64 {
    // Integer rescaling avoids floating-point loss at long media timestamps.
    unsafe { ffi::av_rescale_q(value, from.into(), to.into()) }
}

fn valid_samples(decoded: usize, duration: i64, time_base: Rational, rate: u32) -> usize {
    if duration <= 0 {
        return decoded;
    }
    let numerator = i128::from(duration) * i128::from(time_base.0) * i128::from(rate);
    let denominator = i128::from(time_base.1);
    // Coarse container ticks cannot define exact sample cuts. A tick longer
    // than a sample (ASF and Matroska use milliseconds) means the duration was
    // rounded, even when it happens to be a whole number of samples: at 48 kHz
    // every millisecond is. Do not round a duration down and silently lose
    // samples on every ordinary packet.
    let tick_exceeds_sample = i128::from(time_base.0) * i128::from(rate) > denominator;
    if numerator <= 0 || tick_exceeds_sample || numerator % denominator != 0 {
        return decoded;
    }
    decoded.min((numerator / denominator).min(usize::MAX as i128) as usize)
}

fn grid_skip(position: i64, rate: u32) -> usize {
    let (mut a, mut b) = (rate, SAMPLE_RATE);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    let period = i64::from(rate / a);
    ((period - position.rem_euclid(period)) % period) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn album_fallback_uses_matching_peak_and_off_is_unity() {
        let tags = GainMetadata {
            track_gain: Some(12.0),
            track_peak: Some(0.5),
            album_peak: Some(8.0),
            ..GainMetadata::default()
        };
        let expected = -20.0 * 0.5_f64.log10();
        assert!((tags.selected_db(GainMode::Album) - expected).abs() < 1e-12);
        assert_eq!(tags.selected_db(GainMode::Off), 0.0);
        assert_eq!(GainMetadata::default().selected_db(GainMode::Track), 0.0);
        let album = GainMetadata {
            album_gain: Some(-3.0),
            ..tags
        };
        assert_eq!(album.selected_db(GainMode::Album), -20.0 * 8.0_f64.log10());
    }

    #[test]
    fn tag_numbers_reject_nonfinite_and_allow_case_insensitive_units() {
        assert_eq!(tag_number("  +3.25 DB  "), Some(3.25));
        assert_eq!(tag_number("-4db"), Some(-4.0));
        for value in ["NaN", "inf", "", "loud", "3 dB junk", "é"] {
            assert_eq!(tag_number(value), None);
        }
        assert_eq!(raw_gain(i32::MIN), None);
        assert_eq!(raw_peak(0), None);
        assert_eq!(raw_gain(-325_000), Some(-3.25));
        assert_eq!(raw_peak(50_000), Some(0.5));
    }

    #[test]
    fn duration_only_shortens_when_it_defines_exact_samples() {
        assert_eq!(valid_samples(1024, 137, Rational(1, 48_000), 48_000), 137);
        assert_eq!(valid_samples(137, 137, Rational(1, 48_000), 48_000), 137);
        assert_eq!(valid_samples(1024, 2048, Rational(1, 48_000), 48_000), 1024);
        assert_eq!(valid_samples(1024, 23, Rational(1, 1000), 44_100), 1024);
        assert_eq!(valid_samples(1024, 0, Rational(1, 48_000), 48_000), 1024);
    }

    #[test]
    fn millisecond_ticks_never_shorten_a_frame() {
        // ASF and Matroska round packet durations to whole milliseconds. At
        // 48 kHz every millisecond is a whole number of samples, so a rounded
        // duration looks exact but is not: WMA 2048 samples is 42.67 ms -> 42.
        assert_eq!(valid_samples(2048, 42, Rational(1, 1000), 48_000), 2048);
        assert_eq!(valid_samples(4096, 85, Rational(1, 1000), 48_000), 4096);
        assert_eq!(valid_samples(960, 17, Rational(1, 1000), 48_000), 960);
        assert_eq!(valid_samples(160, 20, Rational(1, 1000), 8_000), 160);
    }

    #[test]
    fn seek_resampling_grid_is_exact_for_positive_and_negative_timestamps() {
        for position in [-148, -147, -1, 0, 1, 146, 147, 999_999] {
            let skip = grid_skip(position, 44_100);
            assert!(skip < 147);
            assert_eq!((position + skip as i64) * 48_000 % 44_100, 0);
            assert_eq!(grid_skip(position, 48_000), 0);
        }
    }
}
