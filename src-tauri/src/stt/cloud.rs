//! Hosted Whisper over OpenAI's `/audio/transcriptions` contract.

use std::time::Duration;

use serde::Deserialize;
use tauri::{AppHandle, Runtime};

use crate::error::{AppError, Result};
use crate::http;
use crate::secrets::{self, Credential};
use crate::settings::{Settings, SttProvider};

const GROQ_URL: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
const OPENAI_URL: &str = "https://api.openai.com/v1/audio/transcriptions";

/// Hard ceiling on one request.
///
/// Well above the p99 for a normal utterance, but finite: without it a hung
/// connection leaves the user staring at a spinner with no way to tell whether
/// their words are coming. On timeout they get an error and can retry.
const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
struct TranscriptionResponse {
    text: String,
}

pub async fn transcribe<R: Runtime>(
    app: &AppHandle<R>,
    settings: &Settings,
    wav: Vec<u8>,
    hints: &[String],
) -> Result<String> {
    let (url, credential, provider) = match settings.providers.stt {
        SttProvider::Groq => (GROQ_URL, Credential::Groq, "Groq"),
        SttProvider::OpenAi => (OPENAI_URL, Credential::OpenAi, "OpenAI"),
        SttProvider::LocalWhisper => {
            return Err(AppError::Settings(
                "local STT is not a cloud provider".into(),
            ))
        }
    };

    let api_key = secrets::get(app, credential).await?;
    let model = settings.providers.stt_model.trim();
    if model.is_empty() {
        return Err(AppError::Settings("no STT model configured".into()));
    }

    // `file_name` matters: these APIs sniff the container from the extension and
    // reject the upload without it, even though the bytes are unambiguous.
    let part = reqwest::multipart::Part::bytes(wav)
        .file_name("speech.wav")
        .mime_str("audio/wav")
        .map_err(|err| AppError::Http(err.to_string()))?;

    let mut form = reqwest::multipart::Form::new()
        .text("model", model.to_string())
        .text("response_format", "json")
        // Pinning the language skips Whisper's detection pass. It is a real
        // latency saving and it stops a short, noisy clip being mistaken for
        // another language and "translated" into nonsense.
        .text("language", settings.providers.stt_language.clone())
        // 0 = deterministic. Dictation should not be creative.
        .text("temperature", "0")
        .part("file", part);

    if let Some(prompt) = super::build_prompt(hints) {
        form = form.text("prompt", prompt);
    }

    let response = http::client()?
        .post(url)
        .bearer_auth(api_key)
        .timeout(TIMEOUT)
        .multipart(form)
        .send()
        .await
        .map_err(|err| AppError::Http(http::describe(&err)))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|err| AppError::Http(http::describe(&err)))?;

    if !status.is_success() {
        return Err(AppError::Provider {
            provider,
            status: status.as_u16(),
            body: http::truncate(&body),
        });
    }

    serde_json::from_str::<TranscriptionResponse>(&body)
        .map(|parsed| parsed.text)
        .map_err(|err| AppError::ProviderShape {
            provider,
            detail: err.to_string(),
        })
}
