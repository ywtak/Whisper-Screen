//! Global hotkey registration.
//!
//! # Why the plugin and not `global-hotkey` directly
//!
//! `global-hotkey` has a hard platform constraint: on Windows the manager must
//! live on a thread running a win32 message loop, and on macOS it must be the
//! *main* thread's run loop. Getting that wrong produces a hotkey that silently
//! never fires. `tauri-plugin-global-shortcut` wraps the same crate and already
//! owns Tauri's event loop on the correct thread, so it is strictly less code
//! and strictly fewer ways to be subtly broken. It costs nothing in latency —
//! the handler is Rust, invoked directly from the OS callback.
//!
//! # The one rule for the handler
//!
//! **Do no work.** The closure below runs on the thread pumping the OS message
//! loop. Anything slow there — a device open, a file read, a lock someone else
//! holds — stalls not just this app but every global shortcut on the system, and
//! on macOS will get the app's event tap killed by the OS watchdog. So the
//! handler does exactly one thing: look up an intent in a read-only map and push
//! a `Trigger` into an unbounded channel. Total cost, a few hundred nanoseconds.
//!
//! Key-down starts the session; key-up ends it or latches it. That is why we
//! need both `Pressed` and `Released` rather than a single toggle event.

use std::collections::HashMap;

use tauri::{AppHandle, Runtime};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use crate::error::{AppError, Result};
use crate::pipeline::{Intent, PipelineHandle, Trigger};
use crate::settings::Settings;

/// What a bound shortcut does.
#[derive(Debug, Clone, Copy)]
enum Binding {
    /// Start/stop a capture session with this intent.
    Session(Intent),
    /// Abandon the in-flight session.
    Cancel,
}

/// Outcome of a registration pass.
///
/// Partial success is the norm, not an edge case: users pick combinations the OS
/// or another app has already claimed. Failing the whole registration because
/// one binding is taken would leave the app with no hotkeys at all, so the
/// failures are reported for the UI to show rather than swallowed.
#[derive(Debug, Default)]
pub struct RegistrationReport {
    pub registered: Vec<String>,
    /// `(accelerator, reason)` for each binding that could not be registered.
    pub failed: Vec<(String, String)>,
}

impl RegistrationReport {
    pub fn is_complete(&self) -> bool {
        self.failed.is_empty()
    }
}

/// Drops every shortcut this app holds. Call before re-registering.
pub fn unregister_all<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    app.global_shortcut()
        .unregister_all()
        .map_err(|err| AppError::BadShortcut {
            shortcut: "<all>".into(),
            reason: err.to_string(),
        })
}

/// Parses the configured accelerators and binds them to the pipeline.
///
/// Safe to call repeatedly — it releases the previous set first.
pub fn register<R: Runtime>(
    app: &AppHandle<R>,
    settings: &Settings,
    pipeline: PipelineHandle,
) -> Result<RegistrationReport> {
    // Ignore a failure here: on first run there is nothing registered yet, and
    // a stale binding is better than refusing to bind anything.
    if let Err(err) = unregister_all(app) {
        log::debug!("nothing to unregister: {err}");
    }

    let candidates = [
        (
            settings.hotkeys.dictate.as_str(),
            Binding::Session(Intent::Dictate),
        ),
        (
            settings.hotkeys.bridge.as_str(),
            Binding::Session(Intent::Bridge),
        ),
        (
            settings.hotkeys.transform.as_str(),
            Binding::Session(Intent::Transform),
        ),
        (settings.hotkeys.cancel.as_str(), Binding::Cancel),
    ];

    let mut report = RegistrationReport::default();
    // Keyed by `Shortcut::id()`, a hash of the modifiers and key. The handler
    // receives the `Shortcut` that fired, and this is the cheapest way to get
    // from that back to our own meaning.
    let mut bindings: HashMap<u32, Binding> = HashMap::new();
    let mut shortcuts: Vec<Shortcut> = Vec::new();

    for (accelerator, binding) in candidates {
        let accelerator = accelerator.trim();
        if accelerator.is_empty() {
            continue; // an intentionally unbound action
        }

        let shortcut: Shortcut = match accelerator.parse() {
            Ok(shortcut) => shortcut,
            Err(err) => {
                log::warn!("cannot parse hotkey {accelerator:?}: {err}");
                report
                    .failed
                    .push((accelerator.to_string(), err.to_string()));
                continue;
            }
        };

        // Two actions bound to the same combination would make the handler's
        // behaviour depend on iteration order. Reject the duplicate instead.
        if bindings.contains_key(&shortcut.id()) {
            report.failed.push((
                accelerator.to_string(),
                "already bound to another action".into(),
            ));
            continue;
        }

        bindings.insert(shortcut.id(), binding);
        shortcuts.push(shortcut);
        report.registered.push(accelerator.to_string());
    }

    if shortcuts.is_empty() {
        return Ok(report);
    }

    // One handler for every shortcut. A closure per binding would mean cloning
    // the pipeline handle N times and would not make dispatch any cheaper.
    let result = app
        .global_shortcut()
        .on_shortcuts(shortcuts, move |_app, shortcut, event| {
            let Some(binding) = bindings.get(&shortcut.id()) else {
                return; // not ours; another registrar's shortcut
            };

            match (binding, event.state()) {
                (Binding::Session(intent), ShortcutState::Pressed) => {
                    pipeline.send(Trigger::Press(*intent));
                }
                (Binding::Session(intent), ShortcutState::Released) => {
                    pipeline.send(Trigger::Release(*intent));
                }
                (Binding::Cancel, ShortcutState::Pressed) => {
                    pipeline.send(Trigger::Cancel);
                }
                // Nothing to do on key-up for cancel.
                (Binding::Cancel, ShortcutState::Released) => {}
            }
        });

    if let Err(err) = result {
        // The plugin registers the batch as a unit, so a conflict on any one
        // combination fails all of them. Report every accelerator as failed
        // rather than claiming a binding we do not hold.
        let reason = err.to_string();
        log::error!("shortcut registration failed: {reason}");
        let claimed: Vec<String> = report.registered.drain(..).collect();
        report
            .failed
            .extend(claimed.into_iter().map(|acc| (acc, reason.clone())));
    }

    log::info!(
        "hotkeys registered: {:?}, failed: {:?}",
        report.registered,
        report.failed
    );
    Ok(report)
}
