//! User settings: one JSON file, owned by Rust.
//!
//! There is deliberately **one** source of truth. The frontend never writes
//! config directly (no `plugin-store` alongside this); it calls `get_settings`
//! / `update_settings` and re-renders from what Rust hands back. That avoids the
//! classic split-brain where the tray UI and the hotkey registrar disagree about
//! which shortcut is bound.
//!
//! Every field carries `#[serde(default)]`, so a config written by an older
//! build — or one a user hand-edited and truncated — still loads instead of
//! bricking the app. `schema_version` exists for the day that isn't enough.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};

use crate::error::{AppError, Result};

pub const SCHEMA_VERSION: u32 = 1;
const FILE_NAME: &str = "settings.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub schema_version: u32,
    pub hotkeys: Hotkeys,
    pub behavior: Behavior,
    pub audio: AudioSettings,
    pub capture: CaptureSettings,
    pub providers: Providers,
    /// Personal dictionary — proper nouns, product names, and jargon the STT
    /// model gets wrong. Fed to the model as a prompt bias in Step 2.
    pub dictionary: Vec<String>,
    /// Spoken triggers that expand to fixed text.
    pub snippets: Vec<Snippet>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Hotkeys {
    /// Flow mode: dictate into the active cursor. No screenshot.
    pub dictate: String,
    /// Multimodal bridge: screenshot + voice into the target web LLM.
    pub bridge: String,
    /// Transform the current selection in place.
    pub transform: String,
    /// Abandon the in-flight session and inject nothing.
    pub cancel: String,
}

impl Default for Hotkeys {
    fn default() -> Self {
        // `Alt+Space` is free on both platforms and reachable with one hand.
        // `CommandOrControl` maps to Cmd on macOS and Ctrl on Windows, but note
        // that Cmd+Space is Spotlight — hence Alt/Option as the base modifier.
        Self {
            dictate: "Alt+Space".into(),
            bridge: "Alt+Shift+Space".into(),
            transform: "Alt+Shift+T".into(),
            // Deliberately *not* bare `Escape`. A global shortcut is registered
            // process-wide, so binding Escape alone would swallow it from every
            // other application on the machine. Registering bare Escape only
            // while a session is live is the right fix and is a Step 2 change to
            // `hotkey::register`; until then this stays a safe combination.
            cancel: "Alt+Escape".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Behavior {
    /// Below this hold duration, a press/release pair latches recording on
    /// (tap-to-toggle) instead of ending it. Above it, the hotkey behaves as
    /// push-to-talk. One binding, both interaction models — this is the single
    /// most important feel decision in a dictation app.
    pub tap_toggle_ms: u64,
    /// Play a short cue when recording starts and stops.
    pub sound_cues: bool,
    /// How long the bridge waits after opening the web target before pasting,
    /// in milliseconds. There is no reliable signal that a browser tab has
    /// finished loading and focused its input, so this is a setting, not a
    /// constant — see `bridge/mod.rs`.
    pub bridge_delay_ms: u64,
}

impl Default for Behavior {
    fn default() -> Self {
        Self {
            tap_toggle_ms: 300,
            sound_cues: true,
            bridge_delay_ms: 2_500,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AudioSettings {
    /// Hold the mic open so recording starts instantly and the pre-roll ring is
    /// populated. Costs a permanently lit OS mic indicator; see `audio::engine`.
    pub prewarm: bool,
    /// How much audio from *before* the keypress to prepend.
    pub preroll_ms: u32,
    /// Hard cap on one utterance. Bounds memory and stops a stuck latch from
    /// recording forever.
    pub max_utterance_secs: u32,
    /// Input device to record from, matched exactly against the OS-reported
    /// device name (see `audio::engine::list_input_devices`). Empty means the
    /// OS default. Honoured when the device is next opened — see the
    /// `audio::engine` module doc.
    pub device_name: String,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            prewarm: true,
            preroll_ms: 400,
            max_utterance_secs: 120,
            device_name: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CaptureSettings {
    /// Longest edge, in pixels, after downscaling. 1568 is Claude's limit and a
    /// good default for GPT-4o/Gemini too. `0` means native resolution.
    pub max_edge_px: u32,
}

impl Default for CaptureSettings {
    fn default() -> Self {
        Self { max_edge_px: 1568 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SttProvider {
    /// Fastest hosted Whisper today; the reason the 800 ms budget is reachable.
    Groq,
    OpenAi,
    /// Bundled `whisper.cpp`. No network, no per-word cost, higher latency.
    LocalWhisper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CleanupProvider {
    Groq,
    OpenAi,
    Anthropic,
    Ollama,
    /// Inject the raw transcript. Saves ~200 ms when you don't want polish.
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WebTarget {
    ChatGpt,
    Claude,
    Gemini,
    Custom { url: String },
}

impl WebTarget {
    pub fn url(&self) -> &str {
        match self {
            Self::ChatGpt => "https://chatgpt.com/",
            Self::Claude => "https://claude.ai/new",
            Self::Gemini => "https://gemini.google.com/app",
            Self::Custom { url } => url,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Providers {
    pub stt: SttProvider,
    pub cleanup: CleanupProvider,
    pub web_target: WebTarget,
    /// The hosted Whisper model to call. Groq's `whisper-large-v3-turbo` is the
    /// only hosted Whisper fast enough for the 800 ms budget.
    pub stt_model: String,
    /// Pinned language code. Skipping Whisper's detection pass is a real
    /// latency saving (see `stt::cloud`).
    pub stt_language: String,
    /// The chat model used for cleanup. One string for all providers, so it
    /// must match `cleanup`: the default pairs with the default Groq provider
    /// (its OpenAI-compatible endpoint). For Anthropic use `claude-haiku-4-5` —
    /// the latency-appropriate choice inside the 800 ms budget. Model IDs are
    /// complete strings; never append a date suffix.
    pub cleanup_model: String,
    /// Path to a `whisper.cpp` GGML/GGUF model file. Only read by the
    /// `local-stt` build; empty means "not configured".
    pub local_model_path: String,
}

impl Default for Providers {
    fn default() -> Self {
        Self {
            stt: SttProvider::Groq,
            cleanup: CleanupProvider::Groq,
            web_target: WebTarget::Claude,
            stt_model: "whisper-large-v3-turbo".into(),
            stt_language: "en".into(),
            cleanup_model: "llama-3.3-70b-versatile".into(),
            local_model_path: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snippet {
    /// What the user says, lowercased and punctuation-stripped before matching.
    pub trigger: String,
    pub expansion: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            hotkeys: Hotkeys::default(),
            behavior: Behavior::default(),
            audio: AudioSettings::default(),
            capture: CaptureSettings::default(),
            providers: Providers::default(),
            dictionary: Vec::new(),
            snippets: Vec::new(),
        }
    }
}

impl Settings {
    /// Clamps anything that would break an invariant elsewhere in the app.
    ///
    /// Called on load and on every update, so a hand-edited config or a UI bug
    /// cannot hand the audio engine a 0-second cap or a 10-minute pre-roll.
    pub fn sanitize(&mut self) {
        self.schema_version = SCHEMA_VERSION;
        self.audio.preroll_ms = self.audio.preroll_ms.min(2_000);
        self.audio.max_utterance_secs = self.audio.max_utterance_secs.clamp(5, 600);
        self.audio.device_name = self.audio.device_name.trim().to_string();
        self.behavior.tap_toggle_ms = self.behavior.tap_toggle_ms.clamp(50, 2_000);
        // 0 is legitimate (browser already open and focused); the cap stops a
        // hand-edit from turning every bridge into a multi-minute hang.
        self.behavior.bridge_delay_ms = self.behavior.bridge_delay_ms.min(15_000);
        if self.capture.max_edge_px != 0 {
            self.capture.max_edge_px = self.capture.max_edge_px.clamp(256, 8_192);
        }
        self.dictionary.retain(|term| !term.trim().is_empty());
        self.snippets
            .retain(|s| !s.trigger.trim().is_empty() && !s.expansion.is_empty());
        if self.providers.cleanup_model.trim().is_empty() {
            self.providers.cleanup_model = Providers::default().cleanup_model;
        }
    }
}

pub fn config_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf> {
    Ok(app.path().app_config_dir()?.join(FILE_NAME))
}

/// Loads settings, falling back to defaults.
///
/// A corrupt file is renamed to `settings.json.bad` rather than deleted or
/// silently overwritten — the user may have typed something into it that they
/// would like back, and losing it without a trace is worse than a stale file on
/// disk.
pub fn load<R: Runtime>(app: &AppHandle<R>) -> Settings {
    let path = match config_path(app) {
        Ok(path) => path,
        Err(err) => {
            log::warn!("cannot resolve config dir, using defaults: {err}");
            return Settings::default();
        }
    };

    if !path.exists() {
        return Settings::default();
    }

    let parsed = fs::read_to_string(&path)
        .map_err(AppError::from)
        .and_then(|raw| serde_json::from_str::<Settings>(&raw).map_err(AppError::from));

    match parsed {
        Ok(mut settings) => {
            settings.sanitize();
            settings
        }
        Err(err) => {
            log::error!(
                "settings at {} are unreadable ({err}); using defaults",
                path.display()
            );
            let quarantine = path.with_extension("json.bad");
            if let Err(err) = fs::rename(&path, &quarantine) {
                log::warn!("could not quarantine the bad settings file: {err}");
            }
            Settings::default()
        }
    }
}

/// Writes settings atomically: serialise to a sibling temp file, then rename.
///
/// A rename is atomic on both NTFS and APFS, so a crash mid-write leaves the
/// previous config intact rather than a half-written file that fails to parse
/// on next launch.
pub fn save<R: Runtime>(app: &AppHandle<R>, settings: &Settings) -> Result<()> {
    let path = config_path(app)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_string_pretty(settings)?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, json)?;
    fs::rename(&temp, &path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partial_config_still_loads() {
        let json = r#"{"hotkeys":{"dictate":"Ctrl+Alt+D"}}"#;
        let settings: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.hotkeys.dictate, "Ctrl+Alt+D");
        // Everything unspecified falls back.
        assert_eq!(settings.hotkeys.bridge, Hotkeys::default().bridge);
        assert!(settings.audio.prewarm);
    }

    #[test]
    fn an_empty_object_loads_as_defaults() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.audio.max_utterance_secs, 120);
    }

    #[test]
    fn sanitize_clamps_hostile_values() {
        let mut settings = Settings::default();
        settings.audio.max_utterance_secs = 0;
        settings.audio.preroll_ms = 999_999;
        settings.behavior.tap_toggle_ms = 0;
        settings.capture.max_edge_px = 12;
        settings.dictionary = vec!["  ".into(), "Supabase".into()];
        settings.sanitize();

        assert_eq!(settings.audio.max_utterance_secs, 5);
        assert_eq!(settings.audio.preroll_ms, 2_000);
        assert_eq!(settings.behavior.tap_toggle_ms, 50);
        assert_eq!(settings.capture.max_edge_px, 256);
        assert_eq!(settings.dictionary, vec!["Supabase".to_string()]);
    }

    #[test]
    fn native_resolution_survives_sanitize() {
        let mut settings = Settings::default();
        settings.capture.max_edge_px = 0;
        settings.sanitize();
        assert_eq!(settings.capture.max_edge_px, 0);
    }

    #[test]
    fn sanitize_restores_an_empty_cleanup_model() {
        let mut settings = Settings::default();
        settings.providers.cleanup_model = "   ".into();
        settings.sanitize();
        assert_eq!(
            settings.providers.cleanup_model,
            Providers::default().cleanup_model
        );
    }

    #[test]
    fn sanitize_caps_the_bridge_delay_but_allows_zero() {
        let mut settings = Settings::default();
        settings.behavior.bridge_delay_ms = 999_999;
        settings.sanitize();
        assert_eq!(settings.behavior.bridge_delay_ms, 15_000);

        let mut settings = Settings::default();
        settings.behavior.bridge_delay_ms = 0;
        settings.sanitize();
        assert_eq!(settings.behavior.bridge_delay_ms, 0);
    }

    #[test]
    fn web_target_round_trips_including_custom() {
        let custom = WebTarget::Custom {
            url: "https://example.test/chat".into(),
        };
        let json = serde_json::to_string(&custom).unwrap();
        assert_eq!(serde_json::from_str::<WebTarget>(&json).unwrap(), custom);
        assert_eq!(WebTarget::Claude.url(), "https://claude.ai/new");
    }
}
