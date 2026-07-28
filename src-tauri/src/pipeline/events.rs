//! Events the backend pushes to the UI.
//!
//! One channel, one tagged union. The alternative — a command per state that the
//! frontend polls — would either lag the recording indicator or burn CPU on a
//! timer, and would let the UI's idea of the state drift from Rust's.
//!
//! Emitting is fire-and-forget by design: if no window is listening (the normal
//! case — the app lives in the tray and the settings window is usually closed),
//! the event is dropped and the pipeline carries on. Nothing in the audio or
//! capture path may depend on a listener existing.

use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime};

use crate::pipeline::session::Intent;

/// The single event name the frontend subscribes to.
pub const PIPELINE_EVENT: &str = "pipeline://event";

/// Stage-by-stage latency, in milliseconds.
///
/// Shipped to the UI on every completed session so the 800 ms budget in the PRD
/// is a number you can watch rather than a claim in a document. `capture_ms` is
/// wall-clock for the screenshot leg, which overlaps `record_ms` — they are not
/// meant to sum to `total_ms`. `finalize_ms` is everything after key-up, so it
/// contains `stt_ms`; the breakdown exists precisely to keep that visible.
#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Timings {
    pub capture_ms: u64,
    pub record_ms: u64,
    /// Wall-clock for the transcription round trip, as measured by `stt`.
    pub stt_ms: u64,
    pub finalize_ms: u64,
    pub total_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum PipelineEvent {
    /// Both legs have been kicked off. Emitted within a millisecond of the
    /// keypress, so the UI can show the recording indicator immediately.
    Armed {
        session_id: u64,
        intent: Intent,
        /// False means the mic had to be opened on demand and the first moments
        /// of speech were probably lost.
        prewarmed: bool,
        capturing_screen: bool,
    },

    /// A short tap latched recording on; it will run until the next press.
    Latched { session_id: u64 },

    /// The screenshot leg finished — typically while the user is still talking.
    CaptureDone {
        session_id: u64,
        width: u32,
        height: u32,
        source_width: u32,
        source_height: u32,
        monitor: String,
        elapsed_ms: u64,
    },

    /// Periodic progress while recording, for the timer and level meter.
    Recording {
        session_id: u64,
        elapsed_ms: u64,
        level: f32,
    },

    /// The hotkey was released; both legs are being joined.
    Finalizing { session_id: u64 },

    /// STT returned. Emitted *before* `Ready` so the UI can show the text while
    /// the later stages (cleanup, injection) are still running — transcription
    /// is the first moment there is anything worth reading.
    Transcribed { session_id: u64, text: String },

    /// Audio, final text and (if requested) the screenshot are in hand.
    ///
    /// `text` is the cleanup output — or the raw transcript when cleanup is
    /// off, was cancelled out from under us, or failed and was degraded. Step 4
    /// hangs injection off this same point.
    Ready {
        session_id: u64,
        intent: Intent,
        /// The post-cleanup text. Differs from `Transcribed`'s only when a
        /// cleanup provider actually rewrote it.
        text: String,
        duration_ms: u64,
        sample_count: usize,
        peak: f32,
        has_image: bool,
        /// The recording hit `max_utterance_secs`.
        truncated: bool,
        timings: Timings,
    },

    Cancelled {
        session_id: u64,
    },

    Failed {
        /// Absent when the failure happened before a session was allocated.
        session_id: Option<u64>,
        message: String,
    },

    /// Nothing in flight. Lets the UI reset without inferring it.
    Idle,
}

impl PipelineEvent {
    /// Emits to every window, logging rather than propagating a failure.
    ///
    /// A dropped UI event must never fail a recording, so this returns `()`.
    pub fn emit<R: Runtime>(&self, app: &AppHandle<R>) {
        if let Err(err) = app.emit(PIPELINE_EVENT, self) {
            log::debug!("could not emit pipeline event: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_tagged_and_camel_cased() {
        let event = PipelineEvent::Armed {
            session_id: 7,
            intent: Intent::Bridge,
            prewarmed: true,
            capturing_screen: true,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "armed");
        assert_eq!(json["sessionId"], 7);
        assert_eq!(json["intent"], "bridge");
        assert_eq!(json["capturingScreen"], true);
    }

    #[test]
    fn a_unit_variant_still_carries_its_tag() {
        let json = serde_json::to_value(&PipelineEvent::Idle).unwrap();
        assert_eq!(json["type"], "idle");
    }

    #[test]
    fn failed_without_a_session_serialises_null() {
        let event = PipelineEvent::Failed {
            session_id: None,
            message: "no input device".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert!(json["sessionId"].is_null());
        assert_eq!(json["message"], "no input device");
    }

    #[test]
    fn transcribed_carries_the_text_for_the_ui() {
        let event = PipelineEvent::Transcribed {
            session_id: 9,
            text: "hello world".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "transcribed");
        assert_eq!(json["sessionId"], 9);
        assert_eq!(json["text"], "hello world");
    }

    #[test]
    fn ready_carries_the_final_text() {
        let event = PipelineEvent::Ready {
            session_id: 9,
            intent: Intent::Dictate,
            text: "Send this to Sarah.".into(),
            duration_ms: 2_000,
            sample_count: 32_000,
            peak: 0.5,
            has_image: false,
            truncated: false,
            timings: Timings::default(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "ready");
        assert_eq!(json["text"], "Send this to Sarah.");
    }

    #[test]
    fn timings_report_the_stt_leg() {
        let timings = Timings {
            capture_ms: 30,
            record_ms: 2_000,
            stt_ms: 220,
            finalize_ms: 260,
            total_ms: 2_260,
        };
        let json = serde_json::to_value(timings).unwrap();
        assert_eq!(json["sttMs"], 220);
        assert_eq!(json["totalMs"], 2_260);
    }
}
