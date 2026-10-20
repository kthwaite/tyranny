//! Stereo graphic equalizer at the engine sample rate, after decoding/ReplayGain.
//! Coefficients and histories stay in f64; PCM is rounded to f32 only at the end.
//! Bypass and an exactly flat curve never touch PCM, including signed zero.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{Frame, SAMPLE_RATE};

pub const BAND_CENTERS_HZ: [f64; 10] = [
    31.25, 62.5, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EqualizerSettings {
    pub enabled: bool,
    pub preamp_db: f64,
    pub bands_db: [f64; 10],
}

impl EqualizerSettings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.preamp_db.is_finite() && (-24.0..=12.0).contains(&self.preamp_db),
            "equalizer preamp must be finite and between -24 and 12 dB"
        );
        for (index, gain) in self.bands_db.iter().enumerate() {
            ensure!(
                gain.is_finite() && (-12.0..=12.0).contains(gain),
                "equalizer band {} must be finite and between -12 and 12 dB",
                index + 1
            );
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Default)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: [f64; 2],
    z2: [f64; 2],
}

impl Biquad {
    fn peaking(center_hz: f64, gain_db: f64) -> Self {
        // RBJ peaking EQ, one-octave bandwidth at half the peak gain. The
        // omega/sin(omega) term compensates bilinear-transform bandwidth warp.
        let omega = std::f64::consts::TAU * center_hz / f64::from(SAMPLE_RATE);
        let (sin, cos) = omega.sin_cos();
        let amplitude = 10.0_f64.powf(gain_db / 40.0);
        let alpha = sin * (std::f64::consts::LN_2 * 0.5 * omega / sin).sinh();
        let a0 = 1.0 + alpha / amplitude;
        Self {
            b0: (1.0 + alpha * amplitude) / a0,
            b1: -2.0 * cos / a0,
            b2: (1.0 - alpha * amplitude) / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha / amplitude) / a0,
            ..Self::default()
        }
    }

    fn process(&mut self, frame: &mut [f64; 2]) {
        for (channel, sample) in frame.iter_mut().enumerate() {
            let input = *sample;
            let output = self.b0 * input + self.z1[channel];
            self.z1[channel] = flush_tiny(self.b1 * input - self.a1 * output + self.z2[channel]);
            self.z2[channel] = flush_tiny(self.b2 * input - self.a2 * output);
            *sample = output;
        }
    }

    fn reset(&mut self) {
        self.z1 = [0.0; 2];
        self.z2 = [0.0; 2];
    }
}

fn flush_tiny(value: f64) -> f64 {
    // Far below audible PCM (-600 dB), but above f32/f64 denormal ranges.
    // Do this per sample, not per block, so chunking cannot change the result.
    if value.abs() < 1.0e-30 { 0.0 } else { value }
}

pub struct Equalizer {
    settings: EqualizerSettings,
    filters: [Biquad; 10],
    active: usize,
    preamp: f64,
    bypass: bool,
}

impl Default for Equalizer {
    fn default() -> Self {
        Self {
            settings: EqualizerSettings::default(),
            filters: [Biquad::default(); 10],
            active: 0,
            preamp: 1.0,
            bypass: true,
        }
    }
}

impl Equalizer {
    /// Apply validated settings to subsequent PCM, without changing buffered PCM.
    /// Actual configuration changes clear histories to avoid carrying a previous
    /// curve's resonance into the new curve; resending the same settings is a no-op.
    pub fn set_settings(&mut self, settings: &EqualizerSettings) {
        if self.settings == *settings {
            return;
        }
        self.settings.clone_from(settings);
        self.active = 0;
        self.bypass = !settings.enabled;
        if self.bypass {
            self.reset();
            return;
        }
        self.preamp = if settings.preamp_db == 0.0 {
            1.0
        } else {
            10.0_f64.powf(settings.preamp_db / 20.0)
        };
        for (&center, &gain) in BAND_CENTERS_HZ.iter().zip(&settings.bands_db) {
            if gain != 0.0 {
                self.filters[self.active] = Biquad::peaking(center, gain);
                self.active += 1;
            }
        }
        self.bypass = self.active == 0 && settings.preamp_db == 0.0;
    }

    /// Clear histories after a discontinuity, not a natural gapless track change.
    pub fn reset(&mut self) {
        for filter in &mut self.filters {
            filter.reset();
        }
    }

    /// Process each newly decoded block once, before it enters the output pool.
    /// Does not allocate, clip, or recompute coefficients in the sample loop.
    pub fn process(&mut self, frames: &mut [Frame]) {
        if self.bypass {
            return;
        }
        for frame in frames {
            let mut sample = [
                f64::from(frame[0]) * self.preamp,
                f64::from(frame[1]) * self.preamp,
            ];
            for filter in &mut self.filters[..self.active] {
                filter.process(&mut sample);
            }
            *frame = [sample[0] as f32, sample[1] as f32];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured(settings: &EqualizerSettings) -> Equalizer {
        settings.validate().unwrap();
        let mut equalizer = Equalizer::default();
        equalizer.set_settings(settings);
        equalizer
    }

    fn curve() -> EqualizerSettings {
        EqualizerSettings {
            enabled: true,
            preamp_db: -6.0,
            bands_db: [6.0, 4.0, 2.0, 0.0, -2.0, -3.0, -2.0, 0.0, 2.0, 4.0],
        }
    }

    fn assert_bits(actual: &[Frame], expected: &[Frame]) {
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.iter().zip(expected) {
            assert_eq!(actual.map(f32::to_bits), expected.map(f32::to_bits));
        }
    }

    #[test]
    fn flat_and_disabled_curves_preserve_every_sample_bit() {
        let original = [
            [0.0, -0.0],
            [1.0, -1.0],
            [f32::MIN_POSITIVE, f32::from_bits(1)],
            [f32::MAX, f32::MIN],
            [f32::from_bits(0x7fc0_1234), f32::NEG_INFINITY],
        ];
        for settings in [
            EqualizerSettings::default(),
            EqualizerSettings {
                enabled: true,
                ..EqualizerSettings::default()
            },
            EqualizerSettings {
                enabled: true,
                preamp_db: -0.0,
                bands_db: [-0.0; 10],
            },
            EqualizerSettings {
                enabled: false,
                ..curve()
            },
        ] {
            let mut equalizer = configured(&settings);
            let mut frames = original;
            equalizer.process(&mut frames);
            assert_bits(&frames, &original);
        }
    }

    #[test]
    fn settings_validate_finite_ranges_and_exact_band_count() {
        EqualizerSettings::default().validate().unwrap();
        for (preamp_db, bands_db) in [(-24.0, [-12.0; 10]), (12.0, [12.0; 10])] {
            EqualizerSettings {
                enabled: true,
                preamp_db,
                bands_db,
            }
            .validate()
            .unwrap();
        }
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -24.01, 12.01] {
            assert!(
                EqualizerSettings {
                    preamp_db: value,
                    ..EqualizerSettings::default()
                }
                .validate()
                .is_err()
            );
        }
        for index in 0..10 {
            for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -12.01, 12.01] {
                let mut settings = EqualizerSettings::default();
                settings.bands_db[index] = value;
                assert!(settings.validate().is_err());
            }
        }
        for count in [0, 9, 11] {
            let value = serde_json::json!({
                "enabled": true,
                "preamp_db": 0.0,
                "bands_db": vec![0.0; count],
            });
            assert!(serde_json::from_value::<EqualizerSettings>(value).is_err());
        }
        let settings = curve();
        let encoded = serde_json::to_value(&settings).unwrap();
        assert_eq!(encoded["preamp_db"], -6.0);
        assert_eq!(encoded["bands_db"].as_array().unwrap().len(), 10);
        assert_eq!(
            serde_json::from_value::<EqualizerSettings>(encoded).unwrap(),
            settings
        );
    }

    #[test]
    fn preamp_alone_is_linear_and_does_not_clip() {
        for preamp_db in [-24.0, -6.0, 12.0] {
            let settings = EqualizerSettings {
                enabled: true,
                preamp_db,
                ..EqualizerSettings::default()
            };
            let mut equalizer = configured(&settings);
            let original = [[0.5, -0.5], [1.0, -1.0], [0.0, -0.0]];
            let mut frames = original;
            equalizer.process(&mut frames);
            let gain = 10.0_f64.powf(preamp_db / 20.0);
            let expected =
                original.map(|frame| frame.map(|sample| (f64::from(sample) * gain) as f32));
            assert_bits(&frames, &expected);
        }
    }

    fn response_db(settings: &EqualizerSettings, frequency: f64) -> f64 {
        // Warm up on the first half of one continuous tone. Restarting a
        // non-integer cycle would inject a transient into the measurement.
        let frames = SAMPLE_RATE as usize * 4;
        let mut tone: Vec<Frame> = (0..frames)
            .map(|index| {
                let phase =
                    std::f64::consts::TAU * frequency * index as f64 / f64::from(SAMPLE_RATE);
                [(phase.sin() * 0.1) as f32; 2]
            })
            .collect();
        let input_power: f64 = tone[frames / 2..]
            .iter()
            .map(|frame| f64::from(frame[0]).powi(2))
            .sum();
        let mut equalizer = configured(settings);
        equalizer.process(&mut tone);
        let output_power: f64 = tone[frames / 2..]
            .iter()
            .map(|frame| f64::from(frame[0]).powi(2))
            .sum();
        10.0 * (output_power / input_power).log10()
    }

    #[test]
    fn every_band_has_its_requested_center_gain_including_preamp() {
        for (index, center) in BAND_CENTERS_HZ.into_iter().enumerate() {
            for gain in [-12.0, 6.0, 12.0] {
                let mut settings = EqualizerSettings {
                    enabled: true,
                    preamp_db: -3.0,
                    ..EqualizerSettings::default()
                };
                settings.bands_db[index] = gain;
                let measured = response_db(&settings, center);
                assert!(
                    (measured - (gain - 3.0)).abs() < 0.01,
                    "{center} Hz, {gain} dB: measured {measured} dB"
                );
            }
        }
    }

    #[test]
    fn peaking_filter_returns_to_unity_away_from_its_band() {
        let mut settings = EqualizerSettings {
            enabled: true,
            ..EqualizerSettings::default()
        };
        settings.bands_db[5] = 12.0;
        for frequency in [31.25, 16000.0] {
            assert!(response_db(&settings, frequency).abs() < 0.05);
        }
    }

    #[test]
    fn peaking_bandwidth_is_one_octave_at_half_gain() {
        let mut settings = EqualizerSettings {
            enabled: true,
            ..EqualizerSettings::default()
        };
        settings.bands_db[5] = 12.0;
        for frequency in [
            1000.0 / std::f64::consts::SQRT_2,
            1000.0 * std::f64::consts::SQRT_2,
        ] {
            let measured = response_db(&settings, frequency);
            assert!(
                (measured - 6.0).abs() < 0.03,
                "{frequency} Hz: {measured} dB"
            );
        }
    }

    #[test]
    fn stereo_histories_are_isolated() {
        for channel in 0..2 {
            let mut equalizer = configured(&curve());
            let mut frames = vec![[0.0; 2]; 4096];
            frames[0][channel] = 0.5;
            equalizer.process(&mut frames);
            assert!(frames.iter().any(|frame| frame[channel] != 0.0));
            assert!(frames.iter().all(|frame| frame[1 - channel] == 0.0));
        }
    }

    #[test]
    fn processing_is_identical_across_chunk_boundaries() {
        let input: Vec<Frame> = (0..8193)
            .map(|index| {
                [
                    ((index * 17 % 113) as f32 - 56.0) / 100.0,
                    (index as f32).sin(),
                ]
            })
            .collect();
        let mut expected = input.clone();
        configured(&curve()).process(&mut expected);
        for chunk in [1, 7, 137, 1024, 4097] {
            let mut equalizer = configured(&curve());
            let mut actual = input.clone();
            for block in actual.chunks_mut(chunk) {
                equalizer.process(block);
            }
            assert_bits(&actual, &expected);
        }
    }

    #[test]
    fn reset_and_curve_changes_clear_history_but_identical_settings_do_not() {
        let settings = curve();
        let mut equalizer = configured(&settings);
        let mut impulse = [[0.5, -0.5]];
        equalizer.process(&mut impulse);
        equalizer.set_settings(&settings);
        let mut tail = [[0.0; 2]; 16];
        equalizer.process(&mut tail);
        assert!(tail.iter().any(|frame| frame[0] != 0.0));
        equalizer.reset();
        let mut silence = [[0.0; 2]; 16];
        equalizer.process(&mut silence);
        assert_eq!(silence, [[0.0; 2]; 16]);
        let mut actual = [[0.5, -0.5]; 32];
        let mut expected = actual;
        equalizer.process(&mut actual);
        configured(&settings).process(&mut expected);
        assert_bits(&actual, &expected);
        let mut changed = settings;
        changed.preamp_db = -12.0;
        equalizer.set_settings(&changed);
        let mut silence = [[0.0; 2]; 16];
        equalizer.process(&mut silence);
        assert_eq!(silence, [[0.0; 2]; 16]);
    }

    #[test]
    fn bypass_retains_curve_and_reenabling_cannot_replay_an_old_tail() {
        let settings = curve();
        let mut equalizer = configured(&settings);
        equalizer.process(&mut [[0.5, -0.5]; 16]);
        let disabled = EqualizerSettings {
            enabled: false,
            ..settings.clone()
        };
        equalizer.set_settings(&disabled);
        let original = [[0.0, -0.0], [0.25, -0.25]];
        let mut actual = original;
        equalizer.process(&mut actual);
        assert_bits(&actual, &original);
        assert_eq!(equalizer.settings.bands_db, settings.bands_db);
        equalizer.set_settings(&settings);
        let mut silence = [[0.0; 2]; 16];
        equalizer.process(&mut silence);
        assert_eq!(silence, [[0.0; 2]; 16]);
        equalizer.process(&mut [[0.5, -0.5]; 16]);
        equalizer.set_settings(&EqualizerSettings {
            enabled: true,
            ..EqualizerSettings::default()
        });
        let mut actual = original;
        equalizer.process(&mut actual);
        assert_bits(&actual, &original);
    }

    #[test]
    fn extreme_curves_remain_finite_and_tails_decay_without_denormals() {
        for gain in [-12.0, 12.0] {
            let settings = EqualizerSettings {
                enabled: true,
                preamp_db: -24.0,
                bands_db: [gain; 10],
            };
            let mut equalizer = configured(&settings);
            equalizer.process(&mut [[0.25, -0.25]]);
            let mut silence = [[0.0; 2]; 1024];
            for _ in 0..1024 {
                silence.fill([0.0; 2]);
                equalizer.process(&mut silence);
                assert!(silence.iter().flatten().all(|sample| sample.is_finite()));
            }
            // Quantized history can retain an inaudible limit cycle; require
            // decay and normal-or-zero PCM, not exact internal zero state.
            assert!(silence.iter().flatten().all(|sample| {
                sample.abs() < 1.0e-20 && (*sample == 0.0 || sample.is_normal())
            }));
        }
    }
}
