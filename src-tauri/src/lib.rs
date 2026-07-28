//! Whisper Screen — a tray-resident voice OS with a multimodal web bridge.
//!
//! This file does wiring only. The interesting decisions live in:
//!
//! * [`pipeline`] — the concurrent supervisor; start here.
//! * [`audio`]    — the thread that owns the microphone.
//! * [`capture`]  — silent screenshots.
//! * [`hotkey`]   — the OS callback, and why it does no work.

mod audio;
mod bridge;
mod capture;
mod commands;
mod error;
mod hotkey;
mod http;
mod inject;
mod llm;
mod pipeline;
mod secrets;
mod settings;
mod stt;

use std::sync::Arc;

use parking_lot::RwLock;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, WindowEvent};

use audio::AudioEngine;
use hotkey::RegistrationReport;
use pipeline::{PipelineHandle, Trigger};
use settings::Settings;

/// Shared handles, available to every command via `State<AppState>`.
///
/// Note what is *not* here: the in-flight session. That is owned solely by the
/// supervisor task (see [`pipeline`]), which is why none of this needs to be
/// locked for correctness — only for interior mutability.
pub struct AppState {
    pub audio: Arc<AudioEngine>,
    pub settings: Arc<RwLock<Settings>>,
    pub pipeline: PipelineHandle,
    pub hotkeys: Arc<RwLock<RegistrationReport>>,
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(
            tauri_plugin_log::Builder::default()
                .level(if cfg!(debug_assertions) {
                    log::LevelFilter::Debug
                } else {
                    log::LevelFilter::Info
                })
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::update_settings,
            commands::runtime_status,
            commands::trigger_session,
            commands::stop_session,
            commands::cancel_session,
            commands::test_capture,
            commands::set_api_key,
            commands::clear_api_key,
            commands::api_key_status,
            commands::list_input_devices,
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            // On macOS a menu-bar utility should not own a Dock tile or steal
            // focus when it launches — and focus matters here, because dictation
            // types into whatever the user was already working in.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let loaded = settings::load(&handle);
            log::info!(
                "settings loaded from {:?}",
                settings::config_path(&handle).ok()
            );

            // The mic thread starts before anything can ask it to record.
            let engine = Arc::new(AudioEngine::spawn(
                loaded.audio.preroll_ms,
                loaded.audio.device_name.clone(),
            ));
            if loaded.audio.prewarm {
                if let Err(err) = engine.prewarm() {
                    // Not fatal: the user may not have granted mic access yet.
                    // `runtime_status` surfaces this to the settings window.
                    log::error!("could not pre-warm the microphone: {err}");
                }
            }

            let settings = Arc::new(RwLock::new(loaded.clone()));
            let pipeline =
                pipeline::spawn(handle.clone(), Arc::clone(&engine), Arc::clone(&settings));

            let report = hotkey::register(&handle, &loaded, pipeline.clone())?;
            if !report.is_complete() {
                log::warn!("some hotkeys could not be bound: {:?}", report.failed);
            }

            app.manage(AppState {
                audio: Arc::clone(&engine),
                settings: Arc::clone(&settings),
                pipeline: pipeline.clone(),
                hotkeys: Arc::new(RwLock::new(report)),
            });

            build_tray(app, pipeline)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the settings window must not quit a tray app — the user
            // expects the hotkeys to keep working. Hide instead, and let the
            // tray's Quit item be the only way out.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn build_tray<R: tauri::Runtime>(
    app: &tauri::App<R>,
    pipeline: PipelineHandle,
) -> tauri::Result<()> {
    let stop = MenuItem::with_id(app, "stop", "Stop recording", true, None::<&str>)?;
    let open = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Whisper Screen", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[&stop, &PredefinedMenuItem::separator(app)?, &open, &quit],
    )?;

    let mut builder = TrayIconBuilder::with_id("main-tray")
        .tooltip("Whisper Screen")
        .menu(&menu)
        // Left-click is reserved for showing the panel; the menu is on
        // right-click, which is the platform convention on both OSes.
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "stop" => pipeline.send(Trigger::Stop),
            "settings" => show_settings(app),
            "quit" => app.exit(0),
            other => log::debug!("unhandled tray menu id: {other}"),
        });

    // A tray with no icon is invisible on Windows, but a missing icon is a
    // packaging problem — not a reason to refuse to start and take the hotkeys
    // down with it.
    match app.default_window_icon().cloned() {
        Some(icon) => builder = builder.icon(icon),
        None => log::error!("no default window icon; the tray will be unlabelled"),
    }

    builder.build(app)?;
    Ok(())
}

fn show_settings<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let Some(window) = app.get_webview_window("main") else {
        log::error!("the main window is missing");
        return;
    };
    let _ = window.show();
    let _ = window.set_focus();
}
