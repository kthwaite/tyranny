//! Stereo parametric EQ at the engine sample rate, after decoding/ReplayGain.
//! Coefficients and histories stay in f64; PCM is rounded to f32 only at the end.
//! Live edits warm a new fixed-coefficient bank, then linearly crossfade outputs.
//! At most two banks run; a third holds only the latest pending configuration.
//! Settled bypass/flat curves never touch PCM, including signed zero.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{Frame, SAMPLE_RATE};

const GRAPHIC_CENTERS_HZ: [f64; 10] = [
    31.25, 62.5, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];
const MAX_BANDS: usize = 16;
const MAX_SECTIONS: usize = MAX_BANDS * 4;
const WARMUP_FRAMES: usize = SAMPLE_RATE as usize / 100;
const FADE_FRAMES: usize = SAMPLE_RATE as usize / 50;
const TRANSITION_FRAMES: usize = WARMUP_FRAMES + FADE_FRAMES;
/// Upper bound after the last live edit, including an already-running transition.
pub const MAX_SETTLE_FRAMES: usize = TRANSITION_FRAMES * 2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterKind {
    #[default]
    Bell,
    LowShelf,
    HighShelf,
    HighPass,
    LowPass,
    Notch,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EqualizerBand {
    pub enabled: bool,
    pub kind: FilterKind,
    pub frequency_hz: f64,
    pub gain_db: f64,
    pub q: f64,
    pub shelf_slope: f64,
    pub slope_db: u8,
}

impl Default for EqualizerBand {
    fn default() -> Self {
        Self {
            enabled: true,
            kind: FilterKind::Bell,
            frequency_hz: 1000.0,
            gain_db: 0.0,
            q: 1.0,
            shelf_slope: 1.0,
            slope_db: 12,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EqualizerSettings {
    pub enabled: bool,
    pub preamp_db: f64,
    pub bands: Vec<EqualizerBand>,
}

impl EqualizerSettings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.preamp_db.is_finite() && (-96.0..=12.0).contains(&self.preamp_db),
            "equalizer preamp must be finite and between -96 and 12 dB"
        );
        ensure!(
            self.bands.len() <= MAX_BANDS,
            "equalizer permits at most 16 bands"
        );
        // Validate inactive fields as well: switching type/bypass must never
        // activate invalid data that was previously accepted into storage.
        for (index, band) in self.bands.iter().enumerate() {
            for (name, value, minimum, maximum) in [
                ("frequency", band.frequency_hz, 20.0, 20000.0),
                ("gain", band.gain_db, -24.0, 24.0),
                ("Q", band.q, 0.1, 18.0),
                ("shelf slope", band.shelf_slope, 0.1, 1.0),
            ] {
                ensure!(
                    value.is_finite() && (minimum..=maximum).contains(&value),
                    "equalizer band {} {name} must be finite and between {minimum} and {maximum}",
                    index + 1
                );
            }
            ensure!(
                matches!(band.slope_db, 12 | 24 | 36 | 48),
                "equalizer band {} pass slope must be 12, 24, 36 or 48 dB/octave",
                index + 1
            );
        }
        Ok(())
    }

    /// Storage migration only: preserve the former one-octave RBJ alpha,
    /// including its bilinear bandwidth correction at the 16 kHz center.
    /// The caller validates the legacy ranges before converting stored data.
    pub fn from_graphic(enabled: bool, preamp_db: f64, bands_db: [f64; 10]) -> Self {
        let bands = GRAPHIC_CENTERS_HZ
            .into_iter()
            .zip(bands_db)
            .map(|(frequency_hz, gain_db)| {
                let omega = std::f64::consts::TAU * frequency_hz / f64::from(SAMPLE_RATE);
                EqualizerBand {
                    frequency_hz,
                    gain_db,
                    q: 1.0 / (2.0 * (std::f64::consts::LN_2 * 0.5 * omega / omega.sin()).sinh()),
                    ..EqualizerBand::default()
                }
            })
            .collect();
        Self {
            enabled,
            preamp_db,
            bands,
        }
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
    fn normalized(b: [f64; 3], a: [f64; 3]) -> Self {
        Self {
            b0: b[0] / a[0],
            b1: b[1] / a[0],
            b2: b[2] / a[0],
            a1: a[1] / a[0],
            a2: a[2] / a[0],
            ..Self::default()
        }
    }

    fn for_band(band: &EqualizerBand, q: f64) -> Self {
        let omega = std::f64::consts::TAU * band.frequency_hz / f64::from(SAMPLE_RATE);
        let (sin, cos) = omega.sin_cos();
        let alpha = sin / (2.0 * q);
        match band.kind {
            FilterKind::Bell => {
                let a = 10.0_f64.powf(band.gain_db / 40.0);
                Self::normalized(
                    [1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a],
                    [1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a],
                )
            }
            FilterKind::Notch => Self::normalized(
                [1.0, -2.0 * cos, 1.0],
                [1.0 + alpha, -2.0 * cos, 1.0 - alpha],
            ),
            FilterKind::LowPass => Self::normalized(
                [(1.0 - cos) * 0.5, 1.0 - cos, (1.0 - cos) * 0.5],
                [1.0 + alpha, -2.0 * cos, 1.0 - alpha],
            ),
            FilterKind::HighPass => Self::normalized(
                [(1.0 + cos) * 0.5, -(1.0 + cos), (1.0 + cos) * 0.5],
                [1.0 + alpha, -2.0 * cos, 1.0 - alpha],
            ),
            FilterKind::LowShelf | FilterKind::HighShelf => {
                let a = 10.0_f64.powf(band.gain_db / 40.0);
                let alpha =
                    sin * 0.5 * ((a + 1.0 / a) * (1.0 / band.shelf_slope - 1.0) + 2.0).sqrt();
                let beta = 2.0 * a.sqrt() * alpha;
                let plus = a + 1.0;
                let minus = a - 1.0;
                if band.kind == FilterKind::LowShelf {
                    Self::normalized(
                        [
                            a * (plus - minus * cos + beta),
                            2.0 * a * (minus - plus * cos),
                            a * (plus - minus * cos - beta),
                        ],
                        [
                            plus + minus * cos + beta,
                            -2.0 * (minus + plus * cos),
                            plus + minus * cos - beta,
                        ],
                    )
                } else {
                    Self::normalized(
                        [
                            a * (plus + minus * cos + beta),
                            -2.0 * a * (minus + plus * cos),
                            a * (plus + minus * cos - beta),
                        ],
                        [
                            plus - minus * cos + beta,
                            2.0 * (minus - plus * cos),
                            plus - minus * cos - beta,
                        ],
                    )
                }
            }
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
    // Per-sample flushing makes processing independent of block boundaries.
    if value.abs() < 1.0e-30 { 0.0 } else { value }
}

struct FilterBank {
    filters: [Biquad; MAX_SECTIONS],
    active: usize,
    preamp: f64,
}

impl Default for FilterBank {
    fn default() -> Self {
        Self {
            filters: [Biquad::default(); MAX_SECTIONS],
            active: 0,
            preamp: 1.0,
        }
    }
}

impl FilterBank {
    fn configure(&mut self, settings: &EqualizerSettings) {
        self.active = 0;
        self.preamp = 1.0;
        if !settings.enabled {
            return;
        }
        if settings.preamp_db != 0.0 {
            self.preamp = 10.0_f64.powf(settings.preamp_db / 20.0);
        }
        for band in &settings.bands {
            if !band.enabled
                || (band.gain_db == 0.0
                    && matches!(
                        band.kind,
                        FilterKind::Bell | FilterKind::LowShelf | FilterKind::HighShelf
                    ))
            {
                continue;
            }
            let pass = matches!(band.kind, FilterKind::LowPass | FilterKind::HighPass);
            let count = if pass {
                usize::from(band.slope_db / 12)
            } else {
                1
            };
            for section in 0..count {
                let q = if pass {
                    // Butterworth pole pairs ordered from lowest to highest Q.
                    // Only the highest-Q pair carries the user's resonance
                    // scale, avoiding exponential resonance with filter order.
                    let angle =
                        (2 * section + 1) as f64 * std::f64::consts::PI / (4 * count) as f64;
                    let butterworth_q = 1.0 / (2.0 * angle.cos());
                    butterworth_q
                        * if section + 1 == count {
                            band.q * std::f64::consts::SQRT_2
                        } else {
                            1.0
                        }
                } else {
                    band.q
                };
                self.filters[self.active] = Biquad::for_band(band, q);
                self.active += 1;
            }
        }
    }

    fn bypass(&self) -> bool {
        self.active == 0 && self.preamp == 1.0
    }

    fn process(&mut self, input: Frame) -> [f64; 2] {
        let mut sample = input.map(|value| f64::from(value) * self.preamp);
        for filter in &mut self.filters[..self.active] {
            filter.process(&mut sample);
        }
        sample
    }

    fn reset(&mut self) {
        for filter in &mut self.filters[..self.active] {
            filter.reset();
        }
    }
}

pub struct Equalizer {
    settings: EqualizerSettings,
    // Indices rotate, not the banks themselves: no bank copies in processing.
    banks: [FilterBank; 3],
    current: usize,
    next: usize,
    pending: usize,
    transition: Option<usize>,
    has_pending: bool,
}

impl Default for Equalizer {
    fn default() -> Self {
        Self {
            settings: EqualizerSettings::default(),
            banks: std::array::from_fn(|_| FilterBank::default()),
            current: 0,
            next: 1,
            pending: 2,
            transition: None,
            has_pending: false,
        }
    }
}

impl Equalizer {
    /// Apply validated settings to subsequently decoded PCM, not buffered PCM.
    /// No-op updates preserve histories. A live transition is never restarted:
    /// rapid edits replace a single pending bank, with bounded work and latency.
    /// Fixed coefficients avoid unstable intermediate pole configurations.
    pub fn set_settings(&mut self, settings: &EqualizerSettings) {
        debug_assert!(settings.validate().is_ok());
        if self.settings == *settings {
            return;
        }
        self.settings.clone_from(settings);
        if self.transition.is_some() {
            self.banks[self.pending].configure(settings);
            self.has_pending = true;
        } else {
            self.banks[self.next].configure(settings);
            self.transition = Some(0);
        }
    }

    /// Discontinuities immediately select the latest requested configuration,
    /// cancel transitions and clear histories. Never call at a gapless boundary.
    pub fn reset(&mut self) {
        if self.has_pending {
            std::mem::swap(&mut self.current, &mut self.pending);
        } else if self.transition.is_some() {
            std::mem::swap(&mut self.current, &mut self.next);
        }
        self.transition = None;
        self.has_pending = false;
        for bank in &mut self.banks {
            bank.reset();
        }
    }

    /// No allocation, coefficient calculation, clipping or bank copying here.
    /// Warm-up is inaudible, followed by a unity-sum linear output crossfade.
    /// Even a stream of edits runs at most two banks, and the last edit settles
    /// within MAX_SETTLE_FRAMES. Bypass becomes bit-exact after settling.
    pub fn process(&mut self, frames: &mut [Frame]) {
        if self.transition.is_none() && self.banks[self.current].bypass() {
            return;
        }
        for frame in frames {
            if let Some(position) = self.transition {
                let incoming = self.banks[self.next].process(*frame);
                if position < WARMUP_FRAMES {
                    if !self.banks[self.current].bypass() {
                        *frame = self.banks[self.current]
                            .process(*frame)
                            .map(|value| value as f32);
                    }
                } else {
                    let outgoing = self.banks[self.current].process(*frame);
                    let mix = (position - WARMUP_FRAMES + 1) as f64 / FADE_FRAMES as f64;
                    *frame = std::array::from_fn(|channel| {
                        ((1.0 - mix) * outgoing[channel] + mix * incoming[channel]) as f32
                    });
                }
                if position + 1 == TRANSITION_FRAMES {
                    std::mem::swap(&mut self.current, &mut self.next);
                    self.transition = if self.has_pending {
                        std::mem::swap(&mut self.next, &mut self.pending);
                        self.has_pending = false;
                        Some(0)
                    } else {
                        None
                    };
                } else {
                    self.transition = Some(position + 1);
                }
            } else if !self.banks[self.current].bypass() {
                *frame = self.banks[self.current]
                    .process(*frame)
                    .map(|value| value as f32);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(band: EqualizerBand) -> EqualizerSettings {
        EqualizerSettings {
            enabled: true,
            preamp_db: 0.0,
            bands: vec![band],
        }
    }

    fn configured(settings: &EqualizerSettings) -> Equalizer {
        settings.validate().unwrap();
        let mut equalizer = Equalizer::default();
        equalizer.set_settings(settings);
        equalizer.reset();
        equalizer
    }

    fn curve() -> EqualizerSettings {
        EqualizerSettings::from_graphic(
            true,
            -6.0,
            [6.0, 4.0, 2.0, 0.0, -2.0, -3.0, -2.0, 0.0, 2.0, 4.0],
        )
    }

    fn assert_bits(actual: &[Frame], expected: &[Frame]) {
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.iter().zip(expected) {
            assert_eq!(actual.map(f32::to_bits), expected.map(f32::to_bits));
        }
    }

    fn power(filter: &Biquad, frequency: f64) -> f64 {
        let omega = std::f64::consts::TAU * frequency / f64::from(SAMPLE_RATE);
        let polynomial = |a: f64, b: f64, c: f64| {
            (a + b * omega.cos() + c * (2.0 * omega).cos()).powi(2)
                + (b * omega.sin() + c * (2.0 * omega).sin()).powi(2)
        };
        polynomial(filter.b0, filter.b1, filter.b2) / polynomial(1.0, filter.a1, filter.a2)
    }

    fn analytical_db(settings: &EqualizerSettings, frequency: f64) -> f64 {
        let mut bank = FilterBank::default();
        bank.configure(settings);
        20.0 * bank.preamp.log10()
            + bank.filters[..bank.active]
                .iter()
                .map(|filter| 10.0 * power(filter, frequency).log10())
                .sum::<f64>()
    }

    fn response_db(settings: &EqualizerSettings, frequency: f64) -> f64 {
        // One continuous tone: discard two seconds for resonant histories,
        // not just the live transition. Never restart phase at measurement.
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
        configured(settings).process(&mut tone);
        let output_power: f64 = tone[frames / 2..]
            .iter()
            .map(|frame| f64::from(frame[0]).powi(2))
            .sum();
        10.0 * (output_power / input_power).log10()
    }

    #[test]
    fn validation_checks_every_field_even_when_inactive() {
        EqualizerSettings::default().validate().unwrap();
        for value in [-96.0, 12.0] {
            EqualizerSettings {
                preamp_db: value,
                ..EqualizerSettings::default()
            }
            .validate()
            .unwrap();
        }
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -96.01, 12.01] {
            assert!(
                EqualizerSettings {
                    preamp_db: value,
                    ..EqualizerSettings::default()
                }
                .validate()
                .is_err()
            );
        }
        for field in 0..4 {
            let (low, high) = [(20.0, 20000.0), (-24.0, 24.0), (0.1, 18.0), (0.1, 1.0)][field];
            for value in [
                f64::NAN,
                f64::INFINITY,
                f64::NEG_INFINITY,
                low - 0.01,
                high + 0.01,
                low,
                high,
            ] {
                let mut band = EqualizerBand {
                    enabled: false,
                    kind: FilterKind::Notch,
                    ..EqualizerBand::default()
                };
                match field {
                    0 => band.frequency_hz = value,
                    1 => band.gain_db = value,
                    2 => band.q = value,
                    _ => band.shelf_slope = value,
                }
                assert_eq!(
                    settings(band).validate().is_ok(),
                    value == low || value == high
                );
            }
        }
        for slope_db in [0, 6, 12, 18, 24, 36, 48, 60, 255] {
            assert_eq!(
                settings(EqualizerBand {
                    slope_db,
                    ..EqualizerBand::default()
                })
                .validate()
                .is_ok(),
                matches!(slope_db, 12 | 24 | 36 | 48)
            );
        }
        for count in [0, 16, 17] {
            assert_eq!(
                EqualizerSettings {
                    bands: vec![EqualizerBand::default(); count],
                    ..EqualizerSettings::default()
                }
                .validate()
                .is_ok(),
                count <= 16
            );
        }
    }

    #[test]
    fn wire_requires_all_fields_and_rejects_legacy_or_unknown_fields() {
        let encoded = serde_json::to_value(curve()).unwrap();
        assert_eq!(
            serde_json::from_value::<EqualizerSettings>(encoded.clone()).unwrap(),
            curve()
        );
        for key in ["enabled", "preamp_db", "bands"] {
            let mut value = encoded.clone();
            value.as_object_mut().unwrap().remove(key);
            assert!(serde_json::from_value::<EqualizerSettings>(value).is_err());
        }
        for key in [
            "enabled",
            "kind",
            "frequency_hz",
            "gain_db",
            "q",
            "shelf_slope",
            "slope_db",
        ] {
            let mut value = encoded.clone();
            value["bands"][0].as_object_mut().unwrap().remove(key);
            assert!(serde_json::from_value::<EqualizerSettings>(value).is_err());
        }
        for key in ["bands_db", "unexpected"] {
            let mut value = encoded.clone();
            value[key] = serde_json::json!([]);
            assert!(serde_json::from_value::<EqualizerSettings>(value).is_err());
        }
        let mut value = encoded;
        value["bands"][0]["unexpected"] = true.into();
        assert!(serde_json::from_value::<EqualizerSettings>(value).is_err());
    }

    #[test]
    fn flat_disabled_and_bypassed_bands_preserve_every_sample_bit() {
        let original = [
            [0.0, -0.0],
            [1.0, -1.0],
            [f32::MIN_POSITIVE, f32::from_bits(1)],
            [f32::MAX, f32::MIN],
            [f32::from_bits(0x7fc0_1234), f32::NEG_INFINITY],
        ];
        for config in [
            EqualizerSettings::default(),
            EqualizerSettings {
                enabled: true,
                ..EqualizerSettings::default()
            },
            EqualizerSettings {
                enabled: false,
                ..curve()
            },
            settings(EqualizerBand::default()),
            settings(EqualizerBand {
                enabled: false,
                kind: FilterKind::Notch,
                ..EqualizerBand::default()
            }),
        ] {
            let mut frames = original;
            configured(&config).process(&mut frames);
            assert_bits(&frames, &original);
        }
    }

    #[test]
    fn preamp_applies_once_and_does_not_clip() {
        for preamp_db in [-96.0, -6.0, 12.0] {
            let config = EqualizerSettings {
                enabled: true,
                preamp_db,
                bands: vec![EqualizerBand::default(); 16],
            };
            let original = [[0.5, -0.5], [1.0, -1.0], [0.0, -0.0]];
            let mut frames = original;
            configured(&config).process(&mut frames);
            let gain = 10.0_f64.powf(preamp_db / 20.0);
            assert_bits(
                &frames,
                &original.map(|frame| frame.map(|sample| (f64::from(sample) * gain) as f32)),
            );
        }
    }

    #[test]
    fn migration_matches_old_formula_at_all_centers_and_probe_frequencies() {
        for gain in [-12.0, 6.0, 12.0] {
            let converted = EqualizerSettings::from_graphic(true, -3.0, [gain; 10]);
            converted.validate().unwrap();
            for (band, center) in converted.bands.iter().zip(GRAPHIC_CENTERS_HZ) {
                let omega = std::f64::consts::TAU * center / f64::from(SAMPLE_RATE);
                let (sin, cos) = omega.sin_cos();
                let a = 10.0_f64.powf(gain / 40.0);
                let alpha = sin * (std::f64::consts::LN_2 * 0.5 * omega / sin).sinh();
                let old = Biquad::normalized(
                    [1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a],
                    [1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a],
                );
                let new = Biquad::for_band(band, band.q);
                for frequency in [
                    20.0,
                    center / 1.5,
                    center,
                    (center * 1.3).min(22000.0),
                    22000.0,
                ] {
                    assert!(
                        (10.0 * (power(&new, frequency) / power(&old, frequency)).log10()).abs()
                            < 1.0e-7,
                        "center={center}, probe={frequency}"
                    );
                }
            }
        }
    }

    #[test]
    fn bell_center_gain_and_q_bandwidth_are_measured() {
        for frequency_hz in [20.0, 1000.0, 16000.0, 20000.0] {
            for gain_db in [-24.0, 6.0, 24.0] {
                let mut config = settings(EqualizerBand {
                    frequency_hz,
                    gain_db,
                    ..EqualizerBand::default()
                });
                config.preamp_db = -3.0;
                assert!((response_db(&config, frequency_hz) - (gain_db - 3.0)).abs() < 0.01);
            }
        }
        let broad = settings(EqualizerBand {
            gain_db: 12.0,
            q: 0.5,
            ..EqualizerBand::default()
        });
        let narrow = settings(EqualizerBand {
            gain_db: 12.0,
            q: 8.0,
            ..EqualizerBand::default()
        });
        assert!(response_db(&broad, 1500.0) > response_db(&narrow, 1500.0) + 8.0);
    }

    #[test]
    fn shelves_have_correct_plateaus_midpoint_and_slope() {
        for kind in [FilterKind::LowShelf, FilterKind::HighShelf] {
            for gain_db in [-24.0, 12.0, 24.0] {
                let config = settings(EqualizerBand {
                    kind,
                    gain_db,
                    ..EqualizerBand::default()
                });
                assert!((response_db(&config, 1000.0) - gain_db * 0.5).abs() < 0.01);
                let (boosted, unity) = if kind == FilterKind::LowShelf {
                    (20.0, 20000.0)
                } else {
                    (20000.0, 20.0)
                };
                assert!((response_db(&config, boosted) - gain_db).abs() < 0.01);
                assert!(response_db(&config, unity).abs() < 0.01);
            }
            let steep = settings(EqualizerBand {
                kind,
                gain_db: 12.0,
                ..EqualizerBand::default()
            });
            let gentle = settings(EqualizerBand {
                shelf_slope: 0.1,
                ..steep.bands[0].clone()
            });
            assert!((response_db(&steep, 500.0) - response_db(&gentle, 500.0)).abs() > 2.0);
        }
    }

    #[test]
    fn pass_orders_are_butterworth_and_q_scales_only_one_section() {
        for kind in [FilterKind::LowPass, FilterKind::HighPass] {
            for slope_db in [12, 24, 36, 48] {
                let config = settings(EqualizerBand {
                    kind,
                    slope_db,
                    q: std::f64::consts::FRAC_1_SQRT_2,
                    ..EqualizerBand::default()
                });
                assert!((response_db(&config, 1000.0) + 10.0 * 2.0_f64.log10()).abs() < 0.01);
                let cutoff = (std::f64::consts::PI * 1000.0 / f64::from(SAMPLE_RATE)).tan();
                for ratio in [0.25, 0.5, 2.0, 4.0] {
                    let frequency =
                        (cutoff * ratio).atan() * f64::from(SAMPLE_RATE) / std::f64::consts::PI;
                    let stop_ratio = if kind == FilterKind::LowPass {
                        ratio
                    } else {
                        1.0 / ratio
                    };
                    let expected = -10.0 * (1.0 + stop_ratio.powi(i32::from(slope_db / 3))).log10();
                    assert!(
                        (response_db(&config, frequency) - expected).abs() < 0.02,
                        "{kind:?}/{slope_db} at {frequency}"
                    );
                }
                let resonant = settings(EqualizerBand {
                    q: 18.0,
                    ..config.bands[0].clone()
                });
                let expected_lift = 20.0 * (18.0 * std::f64::consts::SQRT_2).log10();
                assert!(
                    (analytical_db(&resonant, 1000.0)
                        - analytical_db(&config, 1000.0)
                        - expected_lift)
                        .abs()
                        < 1.0e-8
                );
            }
        }
    }

    #[test]
    fn notch_rejects_center_and_q_controls_width() {
        let wide = settings(EqualizerBand {
            kind: FilterKind::Notch,
            q: 0.5,
            ..EqualizerBand::default()
        });
        let narrow = settings(EqualizerBand {
            q: 18.0,
            ..wide.bands[0].clone()
        });
        assert!(response_db(&wide, 1000.0) < -100.0);
        assert!(response_db(&narrow, 1000.0) < -100.0);
        assert!(response_db(&narrow, 1500.0) - response_db(&wide, 1500.0) > 8.0);
        assert!(response_db(&narrow, 20.0).abs() < 0.01);
    }

    #[test]
    fn channels_are_independent_including_live_transitions() {
        for channel in 0..2 {
            let mut equalizer = Equalizer::default();
            equalizer.set_settings(&curve());
            let mut frames = vec![[0.0; 2]; MAX_SETTLE_FRAMES + 4096];
            for frame in &mut frames[..MAX_SETTLE_FRAMES] {
                frame[channel] = 0.25;
            }
            equalizer.process(&mut frames);
            assert!(frames.iter().any(|frame| frame[channel] != 0.0));
            assert!(frames.iter().all(|frame| frame[1 - channel] == 0.0));
        }
    }

    #[test]
    fn chunking_and_gapless_noop_updates_are_bit_identical() {
        let input: Vec<Frame> = (0..8193)
            .map(|index| {
                [
                    ((index * 17 % 113) as f32 - 56.0) / 100.0,
                    (index as f32).sin(),
                ]
            })
            .collect();
        let config = curve();
        let mut expected = input.clone();
        let mut reference = Equalizer::default();
        reference.set_settings(&config);
        reference.process(&mut expected);
        for chunk in [1, 7, 137, 1024, 4097] {
            let mut equalizer = Equalizer::default();
            equalizer.set_settings(&config);
            let mut actual = input.clone();
            for block in actual.chunks_mut(chunk) {
                equalizer.set_settings(&config);
                equalizer.process(block);
            }
            assert_bits(&actual, &expected);
        }
    }

    #[test]
    fn live_preamp_transition_is_warmed_linear_and_bounded() {
        let config = EqualizerSettings {
            enabled: true,
            preamp_db: -12.0,
            ..EqualizerSettings::default()
        };
        let mut equalizer = Equalizer::default();
        equalizer.set_settings(&config);
        let mut frames = vec![[1.0, -1.0]; TRANSITION_FRAMES + 1];
        equalizer.process(&mut frames);
        assert!(
            frames[..WARMUP_FRAMES]
                .iter()
                .all(|frame| *frame == [1.0, -1.0])
        );
        let gain = 10.0_f64.powf(-12.0 / 20.0);
        for (index, frame) in frames[WARMUP_FRAMES..TRANSITION_FRAMES].iter().enumerate() {
            let fraction = (index + 1) as f64 / FADE_FRAMES as f64;
            let expected = ((1.0 - fraction) + fraction * gain) as f32;
            assert_eq!(*frame, [expected, -expected]);
        }
        assert!(
            frames
                .windows(2)
                .all(|pair| (pair[1][0] - pair[0][0]).abs() < 0.001)
        );
        assert_eq!(frames[TRANSITION_FRAMES], [gain as f32, -gain as f32]);
        assert!(equalizer.transition.is_none());
    }

    #[test]
    fn rapid_edits_coalesce_without_restarting_and_bypass_becomes_exact() {
        let mut equalizer = configured(&curve());
        equalizer.set_settings(&settings(EqualizerBand {
            kind: FilterKind::LowPass,
            slope_db: 48,
            ..EqualizerBand::default()
        }));
        for index in 0..TRANSITION_FRAMES {
            let config = settings(EqualizerBand {
                kind: FilterKind::Notch,
                frequency_hz: 100.0 + index as f64,
                q: 18.0,
                ..EqualizerBand::default()
            });
            equalizer.set_settings(&config);
            let mut frame = [[0.1, -0.1]];
            equalizer.process(&mut frame);
            assert!(
                frame
                    .iter()
                    .flatten()
                    .all(|sample| sample.is_finite() && sample.abs() < 1.0)
            );
        }
        // The original transition completed despite an edit every sample.
        assert_eq!(equalizer.transition, Some(0));
        equalizer.set_settings(&EqualizerSettings::default());
        equalizer.process(&mut vec![[0.1, -0.1]; MAX_SETTLE_FRAMES]);
        assert!(equalizer.transition.is_none());
        assert!(!equalizer.has_pending);
        assert!(equalizer.banks[equalizer.current].bypass());
        let original = [
            [0.0, -0.0],
            [f32::from_bits(1), -0.25],
            [f32::from_bits(0x7fc0_1234), f32::NEG_INFINITY],
        ];
        let mut actual = original;
        equalizer.process(&mut actual);
        assert_bits(&actual, &original);
    }

    #[test]
    fn type_add_remove_and_band_bypass_edits_have_bounded_steps() {
        let mut equalizer = configured(&settings(EqualizerBand {
            gain_db: 6.0,
            ..EqualizerBand::default()
        }));
        let mut previous = 0.1_f32;
        for kind in [
            FilterKind::Bell,
            FilterKind::LowShelf,
            FilterKind::HighShelf,
            FilterKind::HighPass,
            FilterKind::LowPass,
            FilterKind::Notch,
        ] {
            let band = EqualizerBand {
                kind,
                gain_db: 6.0,
                ..EqualizerBand::default()
            };
            for bands in [
                vec![band.clone()],
                vec![band.clone(), band.clone()],
                vec![EqualizerBand {
                    enabled: false,
                    ..band
                }],
                vec![],
            ] {
                equalizer.set_settings(&EqualizerSettings {
                    enabled: true,
                    preamp_db: 0.0,
                    bands,
                });
                let mut frames = vec![[0.1; 2]; MAX_SETTLE_FRAMES];
                equalizer.process(&mut frames);
                for frame in frames {
                    assert!(frame[0].is_finite());
                    assert!(
                        (frame[0] - previous).abs() < 0.02,
                        "{kind:?}: {} -> {}",
                        previous,
                        frame[0]
                    );
                    previous = frame[0];
                }
            }
        }
    }

    #[test]
    fn reset_selects_latest_pending_settings_and_clears_all_tails() {
        let mut equalizer = configured(&curve());
        equalizer.process(&mut [[0.5, -0.5]; 128]);
        equalizer.set_settings(&settings(EqualizerBand {
            kind: FilterKind::Notch,
            ..EqualizerBand::default()
        }));
        let latest = settings(EqualizerBand {
            kind: FilterKind::HighPass,
            slope_db: 48,
            ..EqualizerBand::default()
        });
        equalizer.set_settings(&latest);
        equalizer.reset();
        let mut silence = [[0.0; 2]; 32];
        equalizer.process(&mut silence);
        assert_eq!(silence, [[0.0; 2]; 32]);
        let mut actual = [[0.5, -0.5]; 4096];
        let mut expected = actual;
        equalizer.process(&mut actual);
        configured(&latest).process(&mut expected);
        assert_bits(&actual, &expected);
        assert!(equalizer.transition.is_none());
    }

    #[test]
    fn all_extreme_sections_are_stable_and_impulses_remain_finite() {
        for kind in [
            FilterKind::Bell,
            FilterKind::LowShelf,
            FilterKind::HighShelf,
            FilterKind::HighPass,
            FilterKind::LowPass,
            FilterKind::Notch,
        ] {
            for frequency_hz in [20.0, 20000.0] {
                for q in [0.1, 18.0] {
                    for gain_db in [-24.0, 24.0] {
                        for shelf_slope in [0.1, 1.0] {
                            let config = settings(EqualizerBand {
                                kind,
                                frequency_hz,
                                gain_db,
                                q,
                                shelf_slope,
                                slope_db: 48,
                                ..EqualizerBand::default()
                            });
                            let mut equalizer = configured(&config);
                            for filter in &equalizer.banks[equalizer.current].filters
                                [..equalizer.banks[equalizer.current].active]
                            {
                                // Strict second-order Jury conditions: both poles
                                // inside the unit circle, including complex pairs.
                                assert!(filter.a2.abs() < 1.0);
                                assert!(1.0 + filter.a1 + filter.a2 > 0.0);
                                assert!(1.0 - filter.a1 + filter.a2 > 0.0);
                            }
                            let mut frames = [[0.0; 2]; 4096];
                            frames[0] = [0.25, -0.25];
                            equalizer.process(&mut frames);
                            assert!(frames.iter().flatten().all(|sample| sample.is_finite()));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn maximum_pass_cascades_and_preamp_remain_finite_in_f32() {
        for kind in [FilterKind::LowPass, FilterKind::HighPass] {
            let config = EqualizerSettings {
                enabled: true,
                preamp_db: 12.0,
                bands: vec![
                    EqualizerBand {
                        kind,
                        frequency_hz: 1000.0,
                        q: 18.0,
                        slope_db: 48,
                        ..EqualizerBand::default()
                    };
                    MAX_BANDS
                ],
            };
            let mut equalizer = configured(&config);
            assert_eq!(equalizer.banks[equalizer.current].active, MAX_SECTIONS);
            let mut maximum = 0.0_f32;
            let mut frames = [[0.0; 2]; 1024];
            // Long resonant input exercises all 64 sections simultaneously;
            // the two channels also carry unrelated bounded signals.
            for block in 0..64 {
                for (index, frame) in frames.iter_mut().enumerate() {
                    let position = block * 1024 + index;
                    let phase =
                        std::f64::consts::TAU * 1000.0 * position as f64 / f64::from(SAMPLE_RATE);
                    *frame = [
                        phase.sin() as f32,
                        if position % 97 < 48 { 1.0 } else { -1.0 },
                    ];
                }
                equalizer.process(&mut frames);
                for sample in frames.iter().flatten() {
                    assert!(sample.is_finite(), "{kind:?} overflowed at block {block}");
                    maximum = maximum.max(sample.abs());
                }
            }
            // Do not mistake a muted/clipped implementation for a stable one.
            assert!(maximum > 1.0e10, "{kind:?}: {maximum}");
        }
    }

    #[test]
    fn resonant_tails_decay_without_denormals() {
        let config = settings(EqualizerBand {
            frequency_hz: 1000.0,
            gain_db: 24.0,
            q: 18.0,
            ..EqualizerBand::default()
        });
        let mut equalizer = configured(&config);
        equalizer.process(&mut [[0.25, -0.25]]);
        let mut silence = [[0.0; 2]; 1024];
        for _ in 0..1024 {
            silence.fill([0.0; 2]);
            equalizer.process(&mut silence);
            assert!(silence.iter().flatten().all(|sample| sample.is_finite()));
        }
        assert!(
            silence
                .iter()
                .flatten()
                .all(|sample| sample.abs() < 1.0e-20 && (*sample == 0.0 || sample.is_normal()))
        );
    }
}
