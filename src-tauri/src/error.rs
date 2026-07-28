//! One error type for the whole backend.
//!
//! Third-party error types (cpal, xcap) are flattened to strings at the call
//! site with `.map_err(...)`. That keeps this enum stable when those crates
//! reshuffle their error hierarchies, and it keeps `AppError: Send + Sync`,
//! which is required to move failures across the pipeline's channels.

use serde::{Serialize, Serializer};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("no default audio input device is available")]
    NoInputDevice,

    #[error("the audio engine thread is not running")]
    AudioEngineDown,

    #[error("no audio was captured")]
    EmptyRecording,

    #[error("no monitor available to capture")]
    NoMonitor,

    #[error("session {0} was superseded before it finished")]
    Superseded(u64),

    #[error("audio: {0}")]
    Audio(String),

    #[error("capture: {0}")]
    Capture(String),

    #[error("settings: {0}")]
    Settings(String),

    #[error("invalid shortcut {shortcut:?}: {reason}")]
    BadShortcut { shortcut: String, reason: String },

    #[error("no {label} API key. Paste one in Settings, or set {env_var}.")]
    MissingCredential {
        label: &'static str,
        env_var: &'static str,
    },

    /// A provider returned a non-2xx response. Carries the status and the body
    /// so the UI can show why (bad key, rate limit, model not found) instead of
    /// a generic failure.
    #[error("{provider} returned {status}: {body}")]
    Provider {
        provider: &'static str,
        status: u16,
        body: String,
    },

    #[error("{provider} sent a response we could not read: {detail}")]
    ProviderShape {
        provider: &'static str,
        detail: String,
    },

    #[error("transcript was empty — nothing was said, or the mic captured silence")]
    EmptyTranscript,

    #[error("nothing is selected in the active application")]
    NoSelection,

    #[error("input injection failed: {0}")]
    Inject(String),

    #[error("clipboard: {0}")]
    Clipboard(String),

    #[error("credential store: {0}")]
    Secrets(String),

    #[error("web bridge: {0}")]
    Bridge(String),

    #[error("network: {0}")]
    Http(String),

    #[error(transparent)]
    Tauri(#[from] tauri::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, AppError>;

/// Tauri commands must return a serializable error. Collapse to the `Display`
/// string so the frontend gets a readable message instead of a tagged union it
/// would have to keep in sync with this enum.
impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}
