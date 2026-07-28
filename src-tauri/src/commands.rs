//! The IPC surface. Thin by design — every command either reads state or hands
//! a `Trigger` to the pipeline. No business logic lives here.

use serde::Serialize;
use tauri::{AppHandle, Runtime, State};

use crate::error::{AppError, Result};
use crate::pipeline::{Intent, Trigger};
use crate::secrets::{self, Credential, CredentialStatus};
use crate::settings::{self, Settings};
use crate::{audio, capture, hotkey, AppState};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedBinding {
    pub accelerator: String,
    pub reason: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyStatus {
    pub registered: Vec<String>,
    pub failed: Vec<FailedBinding>,
}

/// Everything the settings window needs to render its status panel in one round
/// trip, so opening the tray popover is a single IPC call rather than five.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStatus {
    pub mic_open: bool,
    pub mic_error: Option<String>,
    pub sample_rate: Option<u32>,
    pub channels: Option<usize>,
    pub active_session: Option<u64>,
    pub hotkeys: HotkeyStatus,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureInfo {
    pub width: u32,
    pub height: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub monitor: String,
    pub elapsed_ms: u64,
    pub bytes: usize,
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings.read().clone()
}

/// Persists settings and re-binds the hotkeys.
///
/// Order matters: sanitize, then swap in memory, then write to disk, then
/// re-register. If registration fails, the settings are still saved and the
/// report tells the UI which accelerator to complain about — the user should not
/// lose an unrelated change because one hotkey was taken.
#[tauri::command]
pub fn update_settings<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, AppState>,
    mut settings: Settings,
) -> Result<HotkeyStatus> {
    settings.sanitize();

    let prewarm_changed = state.settings.read().audio.prewarm != settings.audio.prewarm;
    let prewarm = settings.audio.prewarm;

    *state.settings.write() = settings.clone();
    settings::save(&app, &settings)?;

    if prewarm_changed {
        // Acquire or release the microphone to match the new preference,
        // immediately — the OS mic indicator is the user's feedback that this
        // setting did something.
        let result = if prewarm {
            state.audio.prewarm()
        } else {
            state.audio.release()
        };
        if let Err(err) = result {
            log::error!("could not apply the prewarm change: {err}");
        }
    }

    let report = hotkey::register(&app, &settings, state.pipeline.clone())?;
    let status = to_status(&report);
    *state.hotkeys.write() = report;
    Ok(status)
}

#[tauri::command]
pub fn runtime_status(state: State<'_, AppState>) -> RuntimeStatus {
    let format = state.audio.format();
    RuntimeStatus {
        mic_open: state.audio.is_open(),
        mic_error: state.audio.last_error(),
        sample_rate: format.map(|f| f.sample_rate),
        channels: format.map(|f| f.channels),
        active_session: state.audio.active_session(),
        hotkeys: to_status(&state.hotkeys.read()),
    }
}

/// Starts a session from the UI. Same path the hotkey takes, so the tray button
/// and the keyboard cannot drift out of sync.
#[tauri::command]
pub fn trigger_session(state: State<'_, AppState>, intent: Intent) {
    state.pipeline.send(Trigger::Press(intent));
    // A click has no key-up, so release immediately. The press was shorter than
    // `tap_toggle_ms`, which latches recording on; `stop_session` ends it.
    state.pipeline.send(Trigger::Release(intent));
}

#[tauri::command]
pub fn stop_session(state: State<'_, AppState>) {
    state.pipeline.send(Trigger::Stop);
}

#[tauri::command]
pub fn cancel_session(state: State<'_, AppState>) {
    state.pipeline.send(Trigger::Cancel);
}

/// Takes one screenshot and reports what it cost. Exists so the <50 ms capture
/// budget can be verified on real hardware from the settings window.
#[tauri::command]
pub async fn test_capture(state: State<'_, AppState>) -> Result<CaptureInfo> {
    let max_edge = state.settings.read().capture.max_edge_px;

    // Blocking pool, not the async worker — same rule as the pipeline.
    let frame = tauri::async_runtime::spawn_blocking(move || capture::capture_primary(max_edge))
        .await??;

    Ok(CaptureInfo {
        width: frame.width,
        height: frame.height,
        source_width: frame.source_width,
        source_height: frame.source_height,
        monitor: frame.monitor.clone(),
        elapsed_ms: frame.elapsed_ms,
        bytes: frame.byte_len(),
    })
}

/// Stores an API key in the OS credential store. An empty or whitespace-only
/// `key` clears it. The key goes one way only — the frontend can write it but
/// never read it back; `api_key_status` is the whole of what it can know.
#[tauri::command]
pub async fn set_api_key<R: Runtime>(app: AppHandle<R>, provider: Credential, key: String) -> Result<()> {
    secrets::set(&app, provider, &key).await
}

#[tauri::command]
pub async fn clear_api_key<R: Runtime>(app: AppHandle<R>, provider: Credential) -> Result<()> {
    secrets::clear(&app, provider).await
}

/// Presence plus a masked hint per provider. Never returns key material.
#[tauri::command]
pub async fn api_key_status<R: Runtime>(app: AppHandle<R>) -> Vec<CredentialStatus> {
    secrets::status(&app).await
}

/// Names of the OS's input devices, for the microphone picker. The saved
/// `audio.device_name` is matched against these exactly.
#[tauri::command]
pub async fn list_input_devices() -> Result<Vec<String>> {
    // cpal enumerates the audio graph synchronously — blocking pool, not an
    // async worker, same rule as the pipeline.
    tauri::async_runtime::spawn_blocking(audio::engine::list_input_devices)
        .await
        .map_err(|err| AppError::Audio(format!("device list task failed: {err}")))?
}

fn to_status(report: &hotkey::RegistrationReport) -> HotkeyStatus {
    HotkeyStatus {
        registered: report.registered.clone(),
        failed: report
            .failed
            .iter()
            .map(|(accelerator, reason)| FailedBinding {
                accelerator: accelerator.clone(),
                reason: reason.clone(),
            })
            .collect(),
    }
}
