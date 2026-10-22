//! Chromaprint fingerprints of the first two minutes of a file.
use crate::{
    Frame, SAMPLE_RATE,
    cancel::Cancellation,
    decoder::{Decoder, GainMode},
};
use anyhow::Result;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rusty_chromaprint::{Configuration, FingerprintCompressor, Fingerprinter};
use std::{path::Path, time::Instant};

pub(super) const FINGERPRINT_SECONDS: u64 = 120;
const CHUNK_FRAMES: usize = SAMPLE_RATE as usize;

pub(super) struct Fingerprint {
    pub encoded: String,
    pub duration_s: u32,
}

/// Fingerprints the first `FINGERPRINT_SECONDS` of audio. `None` when the audio is too short to print.
/// A cancel or timeout is an error that wraps a `LookupError`.
pub(super) fn fingerprint(
    path: &Path,
    cancel: &Cancellation,
    deadline: Instant,
) -> Result<Option<Fingerprint>> {
    let config = Configuration::preset_test2();
    let mut printer = Fingerprinter::new(&config);
    printer.start(SAMPLE_RATE, 2)?;
    let mut decoder = Decoder::open(path, GainMode::Off)?;
    let limit = FINGERPRINT_SECONDS * u64::from(SAMPLE_RATE);
    let mut frames: Vec<Frame> = vec![[0.0; 2]; CHUNK_FRAMES];
    let mut samples: Vec<i16> = Vec::with_capacity(CHUNK_FRAMES * 2);
    let mut decoded = 0u64;
    while decoded < limit {
        cancel.check_lookup(deadline)?;
        let want = CHUNK_FRAMES.min((limit - decoded) as usize);
        let count = decoder.read(&mut frames[..want])?;
        if count == 0 {
            break;
        }
        samples.clear();
        samples.extend(
            frames[..count]
                .iter()
                .flatten()
                .map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16),
        );
        printer.consume(&samples);
        decoded += count as u64;
    }
    printer.finish();
    if printer.fingerprint().is_empty() {
        return Ok(None);
    }
    let compressed = FingerprintCompressor::from(&config).compress(printer.fingerprint());
    let total = decoder.duration_frames().unwrap_or(decoded);
    Ok(Some(Fingerprint {
        encoded: URL_SAFE_NO_PAD.encode(compressed),
        duration_s: (total / u64::from(SAMPLE_RATE)) as u32,
    }))
}

#[cfg(test)]
mod tests {
    use super::super::test_support::write_tone_wav;
    use super::*;
    use std::time::Duration;

    struct Remove(std::path::PathBuf);
    impl Drop for Remove {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn fingerprint_of_a_generated_tone_is_url_safe_chromaprint() {
        let path = std::env::temp_dir().join(format!(
            "tyranny-likes-smoke-tone-{}.wav",
            uuid::Uuid::new_v4()
        ));
        write_tone_wav(&path, 10);
        let _remove = Remove(path.clone());
        let print = fingerprint(
            &path,
            &Cancellation::new(),
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap()
        .expect("fingerprint");
        assert_eq!(print.duration_s, 10);
        assert!(print.encoded.starts_with("AQ"), "{}", print.encoded);
        assert!(
            print
                .encoded
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        );
    }
}
