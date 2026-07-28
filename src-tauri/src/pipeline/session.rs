//! Session identity and the intent taxonomy.
//!
//! # Why every stage carries a `session_id`
//!
//! The pipeline has two legs (capture, audio) that finish at unrelated times,
//! plus stages still to come (STT, cleanup, injection) that involve the network.
//! A user who taps the hotkey twice in a second has two sessions alive at once.
//!
//! Rather than trying to prevent that with locks, every stage tags its output
//! with the id it belongs to, and results whose id is no longer current are
//! dropped. This is the same trick React uses for stale renders, and it removes
//! a whole class of "the second recording got pasted into the first's target"
//! bug by construction — no critical section required.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tauri::async_runtime::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::capture::CaptureFrame;
use crate::error::Result;

/// What the user asked for. Determined by *which* hotkey fired, so it is known
/// at t=0 and never has to be inferred later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Intent {
    /// Routing Option 1 — polish and type into the active cursor.
    Dictate,
    /// Routing Option 2 — screenshot + prompt into the target web LLM.
    Bridge,
    /// Module 3 — transform the current selection in place.
    Transform,
}

impl Intent {
    /// Whether this intent needs a screenshot.
    ///
    /// The PRD asks for capture to *always* run in parallel with audio. It does
    /// — but only for the intent that consumes it. Firing DXGI for a Slack
    /// message the user is dictating would burn 30 ms of CPU and read the screen
    /// for no reason, and reading the screen when nothing will look at it is a
    /// privacy cost with no upside. Because the intent is carried by the hotkey
    /// itself, this costs nothing to know up front.
    pub fn needs_screenshot(self) -> bool {
        matches!(self, Self::Bridge)
    }

    /// Whether this intent needs the current selection copied first.
    pub fn needs_selection(self) -> bool {
        matches!(self, Self::Transform)
    }

    /// Whether the final text should be typed into the active cursor.
    ///
    /// Only Dictate delivers by injection — the hotkey was pressed while some
    /// other app had focus, and the text belongs at that app's cursor. Bridge
    /// hands its text (and screenshot) to the target web LLM instead, and
    /// Transform replaces the selection it read, so neither goes through the
    /// generic injection path.
    pub fn injects_text(self) -> bool {
        matches!(self, Self::Dictate)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dictate => "dictate",
            Self::Bridge => "bridge",
            Self::Transform => "transform",
        }
    }
}

/// Messages the supervisor task accepts.
///
/// The hotkey callback's entire job is to construct one of these and send it.
/// It does no work, takes no locks and touches no devices — see `hotkey.rs` for
/// why that matters on Windows.
#[derive(Debug, Clone, Copy)]
pub enum Trigger {
    Press(Intent),
    Release(Intent),
    /// Abandon the in-flight session; inject nothing.
    Cancel,
    /// Stop and process the in-flight session, whatever it is (used by the tray
    /// and by the max-duration timeout).
    Stop,
}

/// Monotonic session id source.
#[derive(Debug)]
pub struct SessionIds(AtomicU64);

impl SessionIds {
    pub fn new() -> Self {
        Self(AtomicU64::new(1))
    }

    pub fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed)
    }
}

impl Default for SessionIds {
    fn default() -> Self {
        Self::new()
    }
}

/// The single in-flight session.
///
/// Owned exclusively by the supervisor task as a plain local `Option<Active>` —
/// **not** behind a mutex. Only one task ever mutates it, so there is nothing to
/// contend on and no lock to accidentally hold across an `.await`.
pub struct Active {
    pub id: u64,
    pub intent: Intent,
    pub started: Instant,
    /// True once a short tap has latched recording on, so the next press stops
    /// it instead of starting a new session.
    pub latched: bool,
    /// Cancels cooperative async stages (the STT request in `finalize`).
    /// Blocking work already in flight cannot be interrupted; its result is
    /// discarded by the session-id check instead.
    pub cancel: CancellationToken,
    /// The screenshot leg, spawned at press time and already running.
    pub capture: Option<JoinHandle<Result<CaptureFrame>>>,
    /// The selection leg (Transform only), spawned at press time for the same
    /// reason as the screenshot: the highlighted text exists at key-*down* and
    /// is usually gone by key-up, so it must be copied while the user's app
    /// still holds it.
    pub selection: Option<JoinHandle<Result<String>>>,
}

impl Active {
    pub fn new(id: u64, intent: Intent) -> Self {
        Self {
            id,
            intent,
            started: Instant::now(),
            latched: false,
            cancel: CancellationToken::new(),
            capture: None,
            selection: None,
        }
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }
}

impl Drop for Active {
    /// Belt and braces against a leaked blocking task.
    ///
    /// `abort` cannot actually interrupt a `spawn_blocking` closure that is
    /// mid-DXGI-call, but it does stop us holding the handle — and therefore the
    /// screenshot's memory — after the session is gone.
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.capture.take() {
            handle.abort();
        }
        if let Some(handle) = self.selection.take() {
            handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_monotonic_and_never_zero() {
        let ids = SessionIds::new();
        let first = ids.next();
        let second = ids.next();
        assert!(first >= 1);
        assert_eq!(second, first + 1);
    }

    #[test]
    fn only_the_bridge_intent_captures_the_screen() {
        assert!(Intent::Bridge.needs_screenshot());
        assert!(!Intent::Dictate.needs_screenshot());
        assert!(!Intent::Transform.needs_screenshot());
    }

    #[test]
    fn only_the_transform_intent_reads_the_selection() {
        assert!(Intent::Transform.needs_selection());
        assert!(!Intent::Dictate.needs_selection());
    }

    #[test]
    fn only_the_dictate_intent_injects_text() {
        assert!(Intent::Dictate.injects_text());
        assert!(!Intent::Bridge.injects_text());
        assert!(!Intent::Transform.injects_text());
    }

    #[test]
    fn intent_serialises_as_camel_case() {
        let json = serde_json::to_string(&Intent::Bridge).unwrap();
        assert_eq!(json, "\"bridge\"");
    }
}
