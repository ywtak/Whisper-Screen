//! The chat-model client: cleanup of raw dictation, and selection transform.
//!
//! Both entry points build a [`Prepared`] (system + user + model) and share
//! one provider round trip ([`complete`]); what differs is the framing and the
//! caller's failure policy. Cleanup degrades to the raw transcript in the
//! pipeline; transform fails the session, because its output is injected over
//! the user's live selection and injecting the wrong thing is destructive.
//!
//! # Why snippets are expanded before the model runs
//!
//! The order of operations is fixed: expand snippets in Rust *first*, then call
//! the model. Expansion is exact (a model asked to substitute a URL will
//! occasionally paraphrase it) and free (no round trip), so only what needs
//! judgement — filler removal, self-correction, spoken formatting — crosses the
//! network. See [`super::prompt`] for the full rule. Transform deliberately
//! skips expansion: the selection is material to be rewritten, not dictation,
//! and the `<<<TEXT` framing must wrap it verbatim.
//!
//! # Why a failed cleanup never loses the transcript
//!
//! Every error here is meant to be degraded by the caller to the raw
//! transcript, the same way `flatten_capture` degrades a failed screenshot:
//! the user keeps their words, slightly rougher. Cleanup is polish, and polish
//! is never worth losing the dictation over — so this module never emits
//! `Failed` itself and `CleanupProvider::None` short-circuits before any work.
//!
//! # Provider shapes
//!
//! Groq, OpenAI and Ollama all speak OpenAI's `/chat/completions` contract, so
//! one request builder covers three — the same trick `stt::cloud` uses for two.
//! Anthropic is a genuinely different shape (top-level `system`, required
//! `max_tokens`, a `content[]` block array in the response, no `temperature`)
//! and gets its own. Model IDs are complete strings; never append a date
//! suffix.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Runtime};

use super::prompt;
use crate::error::{AppError, Result};
use crate::http;
use crate::secrets::{self, Credential};
use crate::settings::{CleanupProvider, Settings};

const GROQ_URL: &str = "https://api.groq.com/openai/v1/chat/completions";
const OPENAI_URL: &str = "https://api.openai.com/v1/chat/completions";
const OLLAMA_URL: &str = "http://localhost:11434/v1/chat/completions";
const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Required by the Anthropic API. 1024 tokens is far past the longest plausible
/// utterance at `max_utterance_secs`, so the cap never truncates real output.
const MAX_TOKENS: u32 = 1024;

/// Hard ceiling on one request, mirroring `stt::cloud`: without it a hung
/// connection leaves the user waiting on polish that will never arrive. On
/// timeout the caller falls back to the raw transcript.
const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

/// The OpenAI `/chat/completions` shape. The system prompt is a message here —
/// contrast [`AnthropicRequest`], where it is a top-level field.
#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: [Message<'a>; 2],
    // 0 = deterministic. Dictation cleanup should not be creative.
    temperature: f32,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ResponseMessage,
}

/// Distinct from the borrowed request-side [`Message`]; serde only sees the
/// field names, so the Rust names are free to differ.
#[derive(Deserialize)]
struct ResponseMessage {
    content: String,
}

/// The Anthropic `/messages` shape. Three deliberate differences from
/// [`ChatRequest`], all load-bearing:
///
/// * `system` is top-level, not a message role.
/// * `max_tokens` is required — omitting it is a 400.
/// * there is **no `temperature` field**: the current flagship models reject
///   sampling parameters outright, so it is not sent to any of them.
#[derive(Serialize)]
struct AnthropicRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: [Message<'a>; 1],
    /// Extended thinking is on by default on some models when this field is
    /// omitted, which is fatal inside an 800 ms dictation budget. Disabled
    /// explicitly so a model swap cannot silently re-enable it.
    thinking: Thinking,
}

#[derive(Serialize)]
struct Thinking {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicBlock>,
}

#[derive(Deserialize)]
struct AnthropicBlock {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

/// Everything a model call needs, assembled before any network I/O. Keeping
/// this pure is what makes the ordering — expand in Rust first, call the model
/// second — unit-testable without a server.
struct Prepared {
    system: String,
    user: String,
    model: String,
}

/// Builds the request inputs, or `None` when cleanup is switched off.
///
/// `CleanupProvider::None` returns the transcript *untouched* — no expansion,
/// no prompt work. That is the documented ~200 ms saving, and it is also the
/// exact behaviour of the caller's fallback when a provider errors.
fn prepare(settings: &Settings, transcript: &str) -> Result<Option<Prepared>> {
    if settings.providers.cleanup == CleanupProvider::None {
        return Ok(None);
    }

    let model = settings.providers.cleanup_model.trim();
    if model.is_empty() {
        return Err(AppError::Settings("no cleanup model configured".into()));
    }

    let snippets: Vec<(String, String)> = settings
        .snippets
        .iter()
        .map(|s| (s.trigger.clone(), s.expansion.clone()))
        .collect();

    Ok(Some(Prepared {
        system: prompt::polish_system(&settings.dictionary),
        user: prompt::expand_snippets(transcript, &snippets),
        model: model.to_string(),
    }))
}

/// Cleans a raw transcript: filler words, self-corrections, spoken formatting.
///
/// `CleanupProvider::None` short-circuits and returns the transcript untouched.
/// Any error from a real provider is for the caller to degrade — the pipeline
/// falls back to the raw transcript rather than failing the session.
pub async fn cleanup<R: Runtime>(
    app: &AppHandle<R>,
    settings: &Settings,
    transcript: &str,
) -> Result<String> {
    let provider = settings.providers.cleanup;
    let Some(prepared) = prepare(settings, transcript)? else {
        return Ok(transcript.to_string());
    };
    complete(app, provider, &prepared).await
}

/// Builds the transform inputs: the spoken instruction against the selection,
/// framed by [`prompt::transform_user`] so the two cannot be confused (the
/// `<<<TEXT` delimiters are prompt-injection defence — do not flatten them).
///
/// No snippet expansion and no `None` short-circuit: there is no "raw
/// fallback" form of a transform, so a missing provider or model is an error
/// the pipeline fails the session on.
fn prepare_transform(settings: &Settings, instruction: &str, selection: &str) -> Result<Prepared> {
    if settings.providers.cleanup == CleanupProvider::None {
        return Err(AppError::Settings(
            "transform needs a chat model, but the cleanup provider is set to none".into(),
        ));
    }

    let model = settings.providers.cleanup_model.trim();
    if model.is_empty() {
        return Err(AppError::Settings("no cleanup model configured".into()));
    }

    Ok(Prepared {
        system: prompt::TRANSFORM_SYSTEM.to_string(),
        user: prompt::transform_user(instruction, selection),
        model: model.to_string(),
    })
}

/// Rewrites `selection` according to the spoken `instruction`.
///
/// Unlike [`cleanup`] there is **no degrade-to-raw fallback**: the caller
/// types the result over the user's live selection, and injecting the wrong
/// thing there is destructive. Any error therefore fails the session rather
/// than degrading.
pub async fn transform<R: Runtime>(
    app: &AppHandle<R>,
    settings: &Settings,
    instruction: &str,
    selection: &str,
) -> Result<String> {
    let provider = settings.providers.cleanup;
    let prepared = prepare_transform(settings, instruction, selection)?;
    complete(app, provider, &prepared).await
}

/// One provider round trip for a prepared system + user pair, shared by
/// [`cleanup`] and [`transform`].
async fn complete<R: Runtime>(
    app: &AppHandle<R>,
    provider: CleanupProvider,
    prepared: &Prepared,
) -> Result<String> {
    let text = match provider {
        CleanupProvider::Groq | CleanupProvider::OpenAi | CleanupProvider::Ollama => {
            chat_completions(app, provider, prepared).await?
        }
        CleanupProvider::Anthropic => anthropic(app, prepared).await?,
        CleanupProvider::None => unreachable!("both callers reject None before any provider work"),
    };

    let text = text.trim().to_string();
    if text.is_empty() {
        // A shape error routes cleanup into the raw-transcript fallback and
        // transform into a session failure — either way an empty response
        // must never replace the user's text.
        return Err(AppError::ProviderShape {
            provider: provider_label(provider),
            detail: "the model returned an empty response".into(),
        });
    }
    Ok(text)
}

pub fn provider_label(provider: CleanupProvider) -> &'static str {
    match provider {
        CleanupProvider::Groq => "Groq",
        CleanupProvider::OpenAi => "OpenAI",
        CleanupProvider::Anthropic => "Anthropic",
        CleanupProvider::Ollama => "Ollama",
        CleanupProvider::None => "none",
    }
}

fn chat_request(prepared: &Prepared) -> ChatRequest<'_> {
    ChatRequest {
        model: &prepared.model,
        messages: [
            Message {
                role: "system",
                content: &prepared.system,
            },
            Message {
                role: "user",
                content: &prepared.user,
            },
        ],
        temperature: 0.0,
    }
}

fn anthropic_request(prepared: &Prepared) -> AnthropicRequest<'_> {
    AnthropicRequest {
        model: &prepared.model,
        max_tokens: MAX_TOKENS,
        system: &prepared.system,
        messages: [Message {
            role: "user",
            content: &prepared.user,
        }],
        thinking: Thinking { kind: "disabled" },
    }
}

/// Groq / OpenAI / Ollama: one shape, differing only in URL and credential
/// (Ollama is local and takes none).
async fn chat_completions<R: Runtime>(
    app: &AppHandle<R>,
    provider: CleanupProvider,
    prepared: &Prepared,
) -> Result<String> {
    let (url, credential) = match provider {
        CleanupProvider::Groq => (GROQ_URL, Some(Credential::Groq)),
        CleanupProvider::OpenAi => (OPENAI_URL, Some(Credential::OpenAi)),
        CleanupProvider::Ollama => (OLLAMA_URL, None),
        _ => unreachable!("only OpenAI-shape providers reach here"),
    };

    let mut request = http::client()?
        .post(url)
        .timeout(TIMEOUT)
        .json(&chat_request(prepared));
    if let Some(credential) = credential {
        let api_key = secrets::get(app, credential).await?;
        request = request.bearer_auth(api_key);
    }

    let response = request
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
            provider: provider_label(provider),
            status: status.as_u16(),
            body: http::truncate(&body),
        });
    }

    read_chat_response(provider_label(provider), &body)
}

/// Anthropic's `/messages` endpoint. Kept separate from [`chat_completions`]
/// rather than forced through one struct: the shapes differ in ways a naive
/// port gets wrong (see [`AnthropicRequest`]).
async fn anthropic<R: Runtime>(app: &AppHandle<R>, prepared: &Prepared) -> Result<String> {
    let api_key = secrets::get(app, Credential::Anthropic).await?;

    let response = http::client()?
        .post(ANTHROPIC_URL)
        .timeout(TIMEOUT)
        .header("x-api-key", api_key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .json(&anthropic_request(prepared))
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
            provider: "Anthropic",
            status: status.as_u16(),
            body: http::truncate(&body),
        });
    }

    read_anthropic_response(&body)
}

/// The response text lives at `choices[0].message.content`.
fn read_chat_response(provider: &'static str, body: &str) -> Result<String> {
    let parsed =
        serde_json::from_str::<ChatResponse>(body).map_err(|err| AppError::ProviderShape {
            provider,
            detail: err.to_string(),
        })?;
    parsed
        .choices
        .into_iter()
        .next()
        .map(|choice| choice.message.content)
        .ok_or_else(|| AppError::ProviderShape {
            provider,
            detail: "the response has no choices".into(),
        })
}

/// The response is a `content[]` block array: read `content[0].text` after
/// checking `content[0].type == "text"`. Anything else — an empty array, or a
/// first block that is not text — is a shape error, not empty output.
fn read_anthropic_response(body: &str) -> Result<String> {
    let parsed = serde_json::from_str::<AnthropicResponse>(body).map_err(|err| {
        AppError::ProviderShape {
            provider: "Anthropic",
            detail: err.to_string(),
        }
    })?;
    match parsed.content.first() {
        Some(AnthropicBlock {
            kind,
            text: Some(text),
        }) if kind == "text" => Ok(text.clone()),
        Some(block) => Err(AppError::ProviderShape {
            provider: "Anthropic",
            detail: format!("first content block is {:?}, expected \"text\"", block.kind),
        }),
        None => Err(AppError::ProviderShape {
            provider: "Anthropic",
            detail: "the response has no content blocks".into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Snippet;

    fn settings_with(provider: CleanupProvider, model: &str) -> Settings {
        let mut settings = Settings::default();
        settings.providers.cleanup = provider;
        settings.providers.cleanup_model = model.into();
        settings
    }

    fn prepared(settings: &Settings, transcript: &str) -> Prepared {
        prepare(settings, transcript).unwrap().unwrap()
    }

    #[test]
    fn none_short_circuits_before_any_preparation() {
        let settings = settings_with(CleanupProvider::None, "");
        assert!(prepare(&settings, "um hi").unwrap().is_none());
    }

    #[test]
    fn snippets_expand_before_the_model_sees_the_text() {
        let mut settings = settings_with(CleanupProvider::Groq, "llama-3.3-70b-versatile");
        settings.snippets.push(Snippet {
            trigger: "insert calendar".into(),
            expansion: "https://cal.example/adam".into(),
        });
        settings.dictionary.push("Supabase".into());

        let prepared = prepared(&settings, "Insert calendar. Thanks");
        // The expansion is already in the user message — the model is never
        // trusted to substitute a URL itself.
        assert_eq!(prepared.user, "https://cal.example/adam Thanks");
        assert!(prepared.system.contains("Supabase"));
    }

    #[test]
    fn an_empty_cleanup_model_is_rejected() {
        let settings = settings_with(CleanupProvider::Groq, "   ");
        assert!(prepare(&settings, "hi").is_err());
    }

    #[test]
    fn openai_shape_puts_the_system_prompt_in_messages() {
        let settings = settings_with(CleanupProvider::Groq, "llama-3.3-70b-versatile");
        let prepared = prepared(&settings, "um hi");
        let body = serde_json::to_value(chat_request(&prepared)).unwrap();

        assert_eq!(body["model"], "llama-3.3-70b-versatile");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "um hi");
        // The Anthropic-only fields must not leak into this shape.
        assert!(body.get("system").is_none());
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn anthropic_shape_has_top_level_system_max_tokens_and_no_temperature() {
        let settings = settings_with(CleanupProvider::Anthropic, "claude-haiku-4-5");
        let prepared = prepared(&settings, "um hi");
        let body = serde_json::to_value(anthropic_request(&prepared)).unwrap();

        assert_eq!(body["model"], "claude-haiku-4-5");
        assert_eq!(body["system"], prompt::polish_system(&[]));
        assert_eq!(body["max_tokens"], 1024);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "um hi");
        // Sampling parameters are rejected by the current flagship models.
        assert!(body.get("temperature").is_none());
        // Extended thinking left implicit can be on by default — fatal inside
        // the latency budget — so it is disabled explicitly.
        assert_eq!(body["thinking"]["type"], "disabled");
    }

    #[test]
    fn transform_framing_uses_the_transform_prompt_and_delimiters() {
        let settings = settings_with(CleanupProvider::Groq, "llama-3.3-70b-versatile");
        let prepared = prepare_transform(&settings, "make it formal", "hey whats up").unwrap();

        assert_eq!(prepared.system, prompt::TRANSFORM_SYSTEM);
        assert_eq!(
            prepared.user,
            prompt::transform_user("make it formal", "hey whats up")
        );
        assert!(prepared.user.contains("<<<TEXT\nhey whats up\nTEXT>>>"));
    }

    #[test]
    fn transform_does_not_expand_snippets_in_the_selection() {
        let mut settings = settings_with(CleanupProvider::Groq, "llama-3.3-70b-versatile");
        settings.snippets.push(Snippet {
            trigger: "insert calendar".into(),
            expansion: "https://cal.example/adam".into(),
        });

        let prepared =
            prepare_transform(&settings, "make it formal", "see insert calendar notes").unwrap();
        // The selection is material to rewrite, not dictation — a trigger
        // inside it must survive verbatim for the model to work on.
        assert!(prepared.user.contains("insert calendar"));
        assert!(!prepared.user.contains("https://cal.example/adam"));
    }

    #[test]
    fn transform_rejects_a_none_provider_and_an_empty_model() {
        let none = settings_with(CleanupProvider::None, "model");
        assert!(prepare_transform(&none, "make it formal", "hey").is_err());

        let empty_model = settings_with(CleanupProvider::Groq, "  ");
        assert!(prepare_transform(&empty_model, "make it formal", "hey").is_err());
    }

    #[test]
    fn chat_response_reads_the_first_choice() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"Send this to Sarah."}}]}"#;
        assert_eq!(
            read_chat_response("Groq", body).unwrap(),
            "Send this to Sarah."
        );
    }

    #[test]
    fn chat_response_without_choices_is_a_shape_error() {
        assert!(read_chat_response("Groq", r#"{"choices":[]}"#).is_err());
    }

    #[test]
    fn anthropic_response_reads_the_first_text_block() {
        let body = r#"{"content":[{"type":"text","text":"Send this to Sarah."}]}"#;
        assert_eq!(read_anthropic_response(body).unwrap(), "Send this to Sarah.");
    }

    #[test]
    fn anthropic_response_rejects_a_non_text_first_block() {
        let body = r#"{"content":[{"type":"thinking","thinking":"hmm"}]}"#;
        assert!(read_anthropic_response(body).is_err());
        assert!(read_anthropic_response(r#"{"content":[]}"#).is_err());
    }
}
