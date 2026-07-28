//! Speech to text.
//!
//! Groq and OpenAI both expose OpenAI's `/audio/transcriptions` shape, so one
//! client covers both and they differ only in base URL, model and credential.
//! Groq is the default because it is the only hosted Whisper fast enough to keep
//! the whole round trip inside the 800 ms budget — typically 150–300 ms for a few
//! seconds of audio, against 600–1200 ms for OpenAI.
//!
//! Local `whisper.cpp` is behind the `local-stt` cargo feature. It is off by
//! default deliberately: `whisper-rs` builds a C++ library, which lengthens the
//! first build considerably and adds a compiler toolchain requirement. Turn it on
//! when you want offline dictation:
//!
//! ```sh
//! cargo build --features local-stt
//! ```

pub mod cloud;

use std::time::Instant;

use tauri::{AppHandle, Runtime};

use crate::error::{AppError, Result};
use crate::settings::{Settings, SttProvider};

/// A transcript plus what it cost, so the UI can attribute latency.
#[derive(Debug, Clone)]
pub struct Transcript {
    pub text: String,
    pub provider: &'static str,
    pub elapsed_ms: u64,
}

/// Transcribes a 16 kHz mono WAV.
///
/// `hints` are the user's personal-dictionary terms. Every Whisper endpoint
/// accepts a `prompt` that biases decoding toward the tokens it contains, which
/// is the cheapest possible fix for "it keeps writing 'super base' instead of
/// 'Supabase'" — no fine-tuning, no post-processing.
pub async fn transcribe<R: Runtime>(
    app: &AppHandle<R>,
    settings: &Settings,
    wav: Vec<u8>,
    hints: &[String],
) -> Result<Transcript> {
    let started = Instant::now();
    let provider = settings.providers.stt;

    let text = match provider {
        SttProvider::Groq | SttProvider::OpenAi => {
            cloud::transcribe(app, settings, wav, hints).await?
        }
        SttProvider::LocalWhisper => local::transcribe(settings, wav, hints).await?,
    };

    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(AppError::EmptyTranscript);
    }

    Ok(Transcript {
        text,
        provider: provider_label(provider),
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

pub fn provider_label(provider: SttProvider) -> &'static str {
    match provider {
        SttProvider::Groq => "Groq",
        SttProvider::OpenAi => "OpenAI",
        SttProvider::LocalWhisper => "whisper.cpp",
    }
}

/// Turns the dictionary into a Whisper decoding prompt.
///
/// Kept short on purpose. Whisper's prompt window is small and a long list
/// crowds out the actual audio context, which makes accuracy *worse* — so this
/// caps at 100 terms and reads as a sentence rather than a bare word list,
/// because that is what the model was trained on.
pub fn build_prompt(hints: &[String]) -> Option<String> {
    let terms: Vec<&str> = hints
        .iter()
        .map(|term| term.trim())
        .filter(|term| !term.is_empty())
        .take(100)
        .collect();

    if terms.is_empty() {
        return None;
    }
    Some(format!(
        "The speaker may use these terms: {}.",
        terms.join(", ")
    ))
}

#[cfg(feature = "local-stt")]
mod local {
    use super::*;
    use crate::audio::TARGET_SAMPLE_RATE;

    /// Runs `whisper.cpp` on the blocking pool.
    ///
    /// Inference is CPU-bound for hundreds of milliseconds to seconds; putting it
    /// on an async worker would stall every other task in the process.
    pub async fn transcribe(settings: &Settings, wav: Vec<u8>, hints: &[String]) -> Result<String> {
        let model_path = settings.providers.local_model_path.clone();
        if model_path.trim().is_empty() {
            return Err(AppError::Settings(
                "set a whisper.cpp model path in Settings to use offline transcription".into(),
            ));
        }
        let prompt = build_prompt(hints);

        tauri::async_runtime::spawn_blocking(move || {
            use whisper_rs::{
                FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters,
            };

            let ctx =
                WhisperContext::new_with_params(&model_path, WhisperContextParameters::default())
                    .map_err(|err| AppError::Audio(format!("load whisper model: {err}")))?;

            let mut state = ctx
                .create_state()
                .map_err(|err| AppError::Audio(format!("whisper state: {err}")))?;

            // Greedy decoding: beam search costs 2-4x the time for a marginal
            // gain on the short, clean utterances this app produces.
            let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            params.set_translate(false);
            params.set_print_progress(false);
            params.set_print_realtime(false);
            params.set_print_special(false);
            if let Some(prompt) = prompt.as_deref() {
                params.set_initial_prompt(prompt);
            }

            let samples = decode_wav_f32(&wav)?;
            state
                .full(params, &samples)
                .map_err(|err| AppError::Audio(format!("whisper inference: {err}")))?;

            let segments = state
                .full_n_segments()
                .map_err(|err| AppError::Audio(format!("whisper segments: {err}")))?;

            let mut text = String::new();
            for i in 0..segments {
                if let Ok(segment) = state.full_get_segment_text(i) {
                    text.push_str(&segment);
                }
            }
            Ok(text)
        })
        .await?
    }

    /// Reverses `audio::encode_wav`: 16-bit PCM back to f32.
    fn decode_wav_f32(wav: &[u8]) -> Result<Vec<f32>> {
        let mut reader = hound::WavReader::new(std::io::Cursor::new(wav))
            .map_err(|err| AppError::Audio(format!("read wav: {err}")))?;

        let spec = reader.spec();
        if spec.sample_rate != TARGET_SAMPLE_RATE || spec.channels != 1 {
            return Err(AppError::Audio(format!(
                "whisper needs 16 kHz mono, got {} Hz / {} ch",
                spec.sample_rate, spec.channels
            )));
        }

        reader
            .samples::<i16>()
            .map(|sample| {
                sample
                    .map(|s| s as f32 / 32_768.0)
                    .map_err(|err| AppError::Audio(format!("wav sample: {err}")))
            })
            .collect()
    }
}

#[cfg(not(feature = "local-stt"))]
mod local {
    use super::*;

    pub async fn transcribe(
        _settings: &Settings,
        _wav: Vec<u8>,
        _hints: &[String],
    ) -> Result<String> {
        Err(AppError::Settings(
            "offline transcription needs a build with `--features local-stt`".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_dictionary_means_no_prompt() {
        assert!(build_prompt(&[]).is_none());
        assert!(build_prompt(&["   ".to_string()]).is_none());
    }

    #[test]
    fn prompt_reads_as_a_sentence() {
        let prompt = build_prompt(&["Supabase".into(), "Vercel".into()]).unwrap();
        assert_eq!(prompt, "The speaker may use these terms: Supabase, Vercel.");
    }

    #[test]
    fn prompt_is_capped_so_it_cannot_crowd_out_the_audio_context() {
        let many: Vec<String> = (0..500).map(|i| format!("term{i}")).collect();
        let prompt = build_prompt(&many).unwrap();
        assert_eq!(prompt.matches(", ").count(), 99);
    }
}
