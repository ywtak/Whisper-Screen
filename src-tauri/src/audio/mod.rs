//! Microphone capture.
//!
//! * [`buffer`]   — the shared sample buffer the realtime callback writes to.
//! * [`engine`]   — the thread that owns the `cpal` stream.
//! * [`resample`] — device rate -> 16 kHz mono.

pub mod buffer;
pub mod engine;
pub mod resample;

pub use engine::{AudioEngine, CapturedAudio, DeviceFormat};
pub use resample::TARGET_SAMPLE_RATE;

use std::io::Cursor;

use crate::error::{AppError, Result};

/// Encodes mono f32 samples as a 16-bit PCM WAV in memory.
///
/// Every hosted STT endpoint (OpenAI, Groq, Deepgram) accepts WAV, and 16-bit
/// PCM is what `whisper.cpp` wants too, so this is the single hand-off format
/// for both the local and remote transcription paths.
///
/// Kept in memory rather than written to a temp file: a 30-second utterance is
/// under 1 MB, and not touching the disk avoids both the latency and the
/// awkward question of where a recording of the user's voice lingers.
pub fn encode_wav(samples: &[f32], sample_rate: u32) -> Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    // 44-byte header + 2 bytes per sample.
    let mut cursor = Cursor::new(Vec::with_capacity(44 + samples.len() * 2));
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec)
            .map_err(|err| AppError::Audio(format!("wav header: {err}")))?;

        for &sample in samples {
            // Clamp before scaling: a hot mic can push past 1.0 and wrapping
            // would turn a loud syllable into a burst of white noise.
            let clamped = sample.clamp(-1.0, 1.0);
            let pcm = (clamped * i16::MAX as f32) as i16;
            writer
                .write_sample(pcm)
                .map_err(|err| AppError::Audio(format!("wav sample: {err}")))?;
        }

        writer
            .finalize()
            .map_err(|err| AppError::Audio(format!("wav finalize: {err}")))?;
    }

    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_has_a_riff_header_and_the_expected_size() {
        let samples = vec![0.0f32; 1600]; // 100 ms at 16 kHz
        let wav = encode_wav(&samples, TARGET_SAMPLE_RATE).unwrap();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(wav.len(), 44 + 1600 * 2);
    }

    #[test]
    fn out_of_range_samples_clamp_instead_of_wrapping() {
        let wav = encode_wav(&[2.0, -2.0], TARGET_SAMPLE_RATE).unwrap();
        let pcm = &wav[44..48];
        let first = i16::from_le_bytes([pcm[0], pcm[1]]);
        let second = i16::from_le_bytes([pcm[2], pcm[3]]);
        assert_eq!(first, i16::MAX);
        assert_eq!(second, -i16::MAX);
    }
}
