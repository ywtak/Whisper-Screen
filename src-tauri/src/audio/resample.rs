//! Device rate -> 16 kHz mono, which is what every Whisper-family model wants.
//!
//! Deliberately dependency-free. Two cases cover essentially all real hardware:
//!
//! * **Integer ratio** (48000/16000 = 3, 32000/16000 = 2): box-filter decimation.
//!   Averaging `ratio` consecutive samples *is* a low-pass, so this is both the
//!   anti-alias filter and the downsample in one pass. Cheap, and close enough
//!   to a sinc resampler on speech that STT accuracy is indistinguishable.
//!
//! * **Non-integer ratio** (44100/16000 = 2.756): pre-average over the window
//!   implied by the ratio, then linearly interpolate. Slightly softer than a
//!   proper polyphase filter, well below the noise floor that matters for STT.
//!
//! If you later need broadcast-quality resampling (e.g. you start doing speaker
//! diarisation or VAD on the raw signal), drop in `rubato`'s `SincFixedIn` behind
//! this same function signature.

pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// Resamples mono `input` from `from_rate` to [`TARGET_SAMPLE_RATE`].
pub fn to_target_rate(input: &[f32], from_rate: u32) -> Vec<f32> {
    resample_mono(input, from_rate, TARGET_SAMPLE_RATE)
}

pub fn resample_mono(input: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if input.is_empty() || from_rate == 0 || to_rate == 0 {
        return Vec::new();
    }
    if from_rate == to_rate {
        return input.to_vec();
    }
    if from_rate < to_rate {
        // Upsampling a 16 kHz-or-lower device. Rare, and linear is fine here
        // because there is no aliasing risk when the rate goes up.
        return interpolate(input, from_rate as f32 / to_rate as f32, 1);
    }
    if from_rate % to_rate == 0 {
        return decimate(input, (from_rate / to_rate) as usize);
    }

    let step = from_rate as f32 / to_rate as f32;
    interpolate(input, step, step.floor().max(1.0) as usize)
}

/// Box-filter decimation by an exact integer factor.
///
/// The trailing chunk may be short; scaling by `factor / chunk.len()` keeps its
/// amplitude consistent with the rest instead of fading the last few samples.
fn decimate(input: &[f32], factor: usize) -> Vec<f32> {
    debug_assert!(factor >= 1);
    input
        .chunks(factor)
        .map(|chunk| chunk.iter().sum::<f32>() / chunk.len() as f32)
        .collect()
}

/// Linear interpolation with a `window`-wide pre-average for anti-aliasing.
fn interpolate(input: &[f32], step: f32, window: usize) -> Vec<f32> {
    let smoothed = if window > 1 {
        moving_average(input, window)
    } else {
        input.to_vec()
    };

    let out_len = ((input.len() as f32) / step).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    let last = smoothed.len() - 1;

    for i in 0..out_len {
        let pos = i as f32 * step;
        let idx = pos as usize;
        if idx >= last {
            out.push(smoothed[last]);
            continue;
        }
        let frac = pos - idx as f32;
        out.push(smoothed[idx] * (1.0 - frac) + smoothed[idx + 1] * frac);
    }
    out
}

/// Centred moving average. Allocates once; `window` is tiny (2–4) in practice.
fn moving_average(input: &[f32], window: usize) -> Vec<f32> {
    let half = window / 2;
    let mut out = Vec::with_capacity(input.len());
    for i in 0..input.len() {
        let start = i.saturating_sub(half);
        let end = (i + half + 1).min(input.len());
        let slice = &input[start..end];
        out.push(slice.iter().sum::<f32>() / slice.len() as f32);
    }
    out
}

/// Peak absolute amplitude, for the UI level meter.
pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |acc, s| acc.max(s.abs()))
}

/// Root-mean-square amplitude — a better "is anyone talking" signal than peak.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_when_rates_match() {
        let input = vec![0.1, 0.2, 0.3];
        assert_eq!(resample_mono(&input, 16_000, 16_000), input);
    }

    #[test]
    fn decimates_48k_to_16k_by_exactly_three() {
        let input: Vec<f32> = (0..300).map(|i| i as f32).collect();
        let out = to_target_rate(&input, 48_000);
        assert_eq!(out.len(), 100);
        // First output is the mean of samples 0, 1, 2.
        assert!((out[0] - 1.0).abs() < 1e-4, "got {}", out[0]);
    }

    #[test]
    fn handles_44100_without_panicking_and_hits_expected_length() {
        let input: Vec<f32> = (0..44_100).map(|i| (i as f32 / 100.0).sin()).collect();
        let out = to_target_rate(&input, 44_100);
        // 44100 -> 16000 over one second.
        assert!(
            (out.len() as i32 - 16_000).abs() <= 2,
            "expected ~16000, got {}",
            out.len()
        );
    }

    #[test]
    fn empty_input_is_empty_output() {
        assert!(to_target_rate(&[], 48_000).is_empty());
    }

    #[test]
    fn dc_level_is_preserved_through_decimation() {
        let input = vec![0.5f32; 300];
        let out = to_target_rate(&input, 48_000);
        assert!(out.iter().all(|s| (s - 0.5).abs() < 1e-5));
    }
}
