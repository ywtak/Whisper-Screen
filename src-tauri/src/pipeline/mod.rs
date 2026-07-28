//! The concurrent pipeline: one supervisor task, two legs, no shared locks.
//!
//! # Shape
//!
//! ```text
//!  OS hotkey (WndProc / CFRunLoop)
//!        |  send(Trigger)          <- non-blocking, no locks, no I/O
//!        v
//!  supervisor task  ---- owns Option<Active> as a *local variable* ----
//!        |
//!        |-- leg 1: audio      -> AudioEngine (dedicated thread, pre-warmed)
//!        |-- leg 2: screenshot -> spawn_blocking(capture_primary)  (Bridge)
//!        |-- leg 2b: selection -> spawn_blocking(read_selection)   (Transform)
//!        v
//!  finalize task (detached) -> join the legs -> STT -> Transcribed -> cleanup
//!                             -> inject (Dictate) / transform+inject (Transform)
//!                             / bridge (Bridge) -> Ready
//! ```
//!
//! # How this avoids races without a mutex
//!
//! The in-flight session lives in a plain `Option<Active>` local to the
//! supervisor task. Exactly one task reads or writes it, so there is no shared
//! mutable state to protect, nothing to contend on, and no chance of holding a
//! lock across an `.await` — the standard way async Rust deadlocks itself.
//!
//! Everything that *must* be shared (the sample buffer) is behind a mutex held
//! for nanoseconds at a time, and every result carries the `session_id` it
//! belongs to so a late arrival from a superseded session is discarded rather
//! than mistaken for the current one.
//!
//! # How this avoids leaks
//!
//! * The mic device is owned by one thread for the process lifetime; sessions
//!   never open or close it, so they cannot leak it.
//! * Sample buffers are capacity-capped (`max_utterance_secs`); a session that
//!   is never closed stops growing instead of exhausting memory.
//! * `Active::drop` cancels its token and aborts the capture handle, so
//!   dropping a session releases the screenshot's memory even if nobody awaited
//!   it.
//! * Finalization is detached, so the supervisor never blocks — and because the
//!   detached task owns the `Active` it was handed, its resources are freed when
//!   it returns, whatever the outcome.
//!
//! # Where the concurrency actually is
//!
//! Note carefully: the screenshot is spawned *at key-down* and runs while the
//! user is still speaking. By the time the key comes up, the screenshot leg has
//! almost always already finished (~30 ms of work against seconds of speech).
//! So `join!` in `finalize` is not where the parallelism happens — it is just
//! where the two already-running legs are collected. Spawning the screenshot at
//! key-*up* instead would serialise it behind the recording and add its full
//! cost to the visible latency.

pub mod events;
pub mod session;

use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use tauri::async_runtime::{self};
use tauri::{AppHandle, Runtime};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::audio::{encode_wav, AudioEngine, CapturedAudio};
use crate::bridge;
use crate::capture::{self, CaptureFrame};
use crate::error::{AppError, Result};
use crate::inject;
use crate::llm;
use crate::settings::Settings;
use crate::stt;

pub use events::{PipelineEvent, Timings, PIPELINE_EVENT};
pub use session::{Active, Intent, SessionIds, Trigger};

/// How often the UI gets a `Recording` tick. 100 ms is smooth enough for a
/// level meter and cheap enough to ignore.
const TICK: Duration = Duration::from_millis(100);

/// The only way in. Cloneable, `Send + Sync`, and cheap to call from anywhere —
/// including an OS callback on a thread we do not control.
#[derive(Clone)]
pub struct PipelineHandle {
    tx: UnboundedSender<Trigger>,
}

impl PipelineHandle {
    /// Queues a trigger. Never blocks, never takes a lock.
    ///
    /// Unbounded on purpose: this is called from the OS hotkey callback, and on
    /// Windows that runs on the thread pumping the win32 message loop. A bounded
    /// channel could make that callback wait on the supervisor, which would
    /// stall the message pump and, with it, every other global shortcut on the
    /// system. Triggers are a few bytes and arrive at human speed, so unbounded
    /// carries no real memory risk.
    pub fn send(&self, trigger: Trigger) {
        if self.tx.send(trigger).is_err() {
            log::error!("pipeline supervisor is gone; dropping {trigger:?}");
        }
    }
}

/// Starts the supervisor task and returns its handle.
pub fn spawn<R: Runtime>(
    app: AppHandle<R>,
    audio: Arc<AudioEngine>,
    settings: Arc<RwLock<Settings>>,
) -> PipelineHandle {
    let (tx, mut rx) = mpsc::unbounded_channel::<Trigger>();
    let ids = SessionIds::new();

    async_runtime::spawn(async move {
        let mut active: Option<Active> = None;
        // Clone of the in-flight finalize's cancel token. Once `Active` moves
        // into the detached finalize task, dropping it here is no longer
        // possible — this handle is the only way `Trigger::Cancel` can still
        // abort that task's STT request.
        let mut finalizing: Option<CancellationToken> = None;
        let mut ticker = tokio::time::interval(TICK);
        // If the runtime is briefly busy, skip missed ticks rather than firing a
        // burst of them at the UI.
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                trigger = rx.recv() => {
                    let Some(trigger) = trigger else {
                        break; // all senders dropped: the app is shutting down
                    };
                    handle_trigger(&app, &audio, &settings, &ids, &mut active, &mut finalizing, trigger);
                }

                _ = ticker.tick() => {
                    tick(&app, &audio, &settings, &mut active, &mut finalizing);
                }
            }
        }

        log::info!("pipeline supervisor exiting");
    });

    PipelineHandle { tx }
}

fn handle_trigger<R: Runtime>(
    app: &AppHandle<R>,
    audio: &Arc<AudioEngine>,
    settings: &Arc<RwLock<Settings>>,
    ids: &SessionIds,
    active: &mut Option<Active>,
    finalizing: &mut Option<CancellationToken>,
    trigger: Trigger,
) {
    match trigger {
        Trigger::Press(intent) => {
            // A press while latched means "stop and process".
            if active.as_ref().is_some_and(|s| s.latched) {
                let current = active.take().expect("checked just above");
                detach_finalize(app, audio, settings, current, finalizing);
                return;
            }

            // A press during an unlatched session supersedes it. Losing that
            // audio is the correct outcome: the user is starting over.
            if let Some(previous) = active.take() {
                audio.discard_session(previous.id);
                PipelineEvent::Cancelled {
                    session_id: previous.id,
                }
                .emit(app);
                // `Drop` cancels the token and aborts the capture handle.
            }

            start(app, audio, settings, ids, active, intent);
        }

        Trigger::Release(intent) => {
            let Some(current) = active.as_mut() else {
                return; // stray key-up, e.g. the key that latched a session
            };
            if current.intent != intent || current.latched {
                return;
            }

            let tap_toggle_ms = settings.read().behavior.tap_toggle_ms;

            if current.elapsed_ms() < tap_toggle_ms {
                // Short tap: latch recording on. One binding gives both
                // push-to-talk and tap-to-toggle, which is what makes the
                // interaction feel right for both a two-word correction and a
                // two-minute monologue.
                current.latched = true;
                let session_id = current.id;
                PipelineEvent::Latched { session_id }.emit(app);
            } else {
                let current = active.take().expect("checked above");
                detach_finalize(app, audio, settings, current, finalizing);
            }
        }

        Trigger::Cancel => {
            if let Some(current) = active.take() {
                audio.discard_session(current.id);
                PipelineEvent::Cancelled {
                    session_id: current.id,
                }
                .emit(app);
            }
            // The detached finalize owns its `Active`, so its token cannot be
            // cancelled by a drop here — cancel our clone instead. Its STT
            // request selects on this and aborts mid-flight.
            if let Some(token) = finalizing.take() {
                token.cancel();
            }
            PipelineEvent::Idle.emit(app);
        }

        Trigger::Stop => {
            if let Some(current) = active.take() {
                detach_finalize(app, audio, settings, current, finalizing);
            }
        }
    }
}

/// Kicks off both legs. Everything here is measured in microseconds — no I/O
/// waits, no device opens, no image work — so the two legs start effectively
/// simultaneously.
fn start<R: Runtime>(
    app: &AppHandle<R>,
    audio: &Arc<AudioEngine>,
    settings: &Arc<RwLock<Settings>>,
    ids: &SessionIds,
    active: &mut Option<Active>,
    intent: Intent,
) {
    // Take one short read of settings and copy out what we need. Holding this
    // guard any longer would risk it crossing an await further down.
    let (max_utterance_secs, use_preroll, max_edge_px) = {
        let settings = settings.read();
        (
            settings.audio.max_utterance_secs,
            settings.audio.prewarm,
            settings.capture.max_edge_px,
        )
    };

    let id = ids.next();
    let mut session = Active::new(id, intent);

    // --- Leg 1: audio. Deliberately first: with the device pre-warmed this is
    // a single uncontended mutex, so the mic is live before we spend a single
    // instruction on the screenshot.
    let prewarmed = audio.is_open();
    if let Err(err) = audio.start_session(id, max_utterance_secs, use_preroll) {
        PipelineEvent::Failed {
            session_id: Some(id),
            message: err.to_string(),
        }
        .emit(app);
        return;
    }

    // --- Leg 2: screenshot. `spawn_blocking` hands it to Tokio's blocking pool
    // (warm threads, no spawn cost) so the DXGI/CoreGraphics call cannot stall
    // an async worker — and cannot stall leg 1, which is already recording.
    let capturing_screen = intent.needs_screenshot();
    if capturing_screen {
        session.capture = Some(async_runtime::spawn_blocking(move || {
            capture::capture_primary(max_edge_px)
        }));
    }

    // --- Leg 2b: selection (Transform only). Same arm-time pattern as the
    // screenshot: the highlighted text exists at key-*down* and is usually
    // gone by key-up, and the copy chord has to land in the app that has
    // focus *now* — so the read cannot wait for finalize. See
    // `capture::selection` for the full reasoning.
    if intent.needs_selection() {
        session.selection = Some(async_runtime::spawn_blocking(capture::read_selection));
    }

    PipelineEvent::Armed {
        session_id: id,
        intent,
        prewarmed,
        capturing_screen,
    }
    .emit(app);

    *active = Some(session);
}

/// Runs finalization on its own task so the supervisor stays responsive.
///
/// This matters more than it looks: finalization includes an STT round trip of
/// a few hundred milliseconds. If the supervisor awaited it inline, every
/// hotkey press during that time would sit unprocessed in the channel.
fn detach_finalize<R: Runtime>(
    app: &AppHandle<R>,
    audio: &Arc<AudioEngine>,
    settings: &Arc<RwLock<Settings>>,
    active: Active,
    finalizing: &mut Option<CancellationToken>,
) {
    let app = app.clone();
    let audio = Arc::clone(audio);
    let settings = Arc::clone(settings);
    // Hand the supervisor a clone of the token before `Active` moves into the
    // task — from here on this clone is the only way `Trigger::Cancel` can
    // reach the in-flight STT request.
    *finalizing = Some(active.cancel.clone());
    async_runtime::spawn(async move {
        finalize(app, audio, settings, active).await;
    });
}

async fn finalize<R: Runtime>(
    app: AppHandle<R>,
    audio: Arc<AudioEngine>,
    settings: Arc<RwLock<Settings>>,
    mut active: Active,
) {
    let session_id = active.id;
    let intent = active.intent;
    let record_ms = active.elapsed_ms();
    let started = active.started;

    PipelineEvent::Finalizing { session_id }.emit(&app);

    // Take the handles out so `Active::drop` doesn't abort the tasks we are
    // about to await.
    let capture_handle = active.capture.take();
    let selection_handle = active.selection.take();

    // Draining and resampling is pure CPU — a few ms for a long utterance — so
    // it goes to the blocking pool rather than an async worker.
    let engine = Arc::clone(&audio);
    let audio_task = async_runtime::spawn_blocking(move || engine.finish_session(session_id));

    // The join. In the common case the legs spawned at key-down resolved
    // seconds ago and this returns immediately; on a very short tap they
    // genuinely overlap here. (No intent takes both a screenshot and a
    // selection today; the join does not rely on that.)
    let (frame, selection, audio_result) = if capture_handle.is_some() || selection_handle.is_some()
    {
        let capture_leg = async {
            match capture_handle {
                Some(handle) => Some(handle.await),
                None => None,
            }
        };
        let selection_leg = async {
            match selection_handle {
                Some(handle) => Some(handle.await),
                None => None,
            }
        };
        let (capture_result, selection_result, audio_result) =
            tokio::join!(capture_leg, selection_leg, audio_task);
        let frame = capture_result.and_then(|result| flatten_capture(session_id, result));
        (frame, selection_result, audio_result)
    } else {
        (None, None, audio_task.await)
    };

    let mut captured: CapturedAudio = match audio_result {
        Ok(Ok(captured)) => captured,
        // The session was replaced or cancelled while we were finalizing.
        // Correct outcome, not an error.
        Ok(Err(AppError::Superseded(_))) => {
            PipelineEvent::Cancelled { session_id }.emit(&app);
            return;
        }
        Ok(Err(err)) => {
            log::warn!("session {session_id} produced no usable audio: {err}");
            PipelineEvent::Failed {
                session_id: Some(session_id),
                message: err.to_string(),
            }
            .emit(&app);
            return;
        }
        Err(err) => {
            // Join error: the blocking task panicked or was aborted.
            log::error!("session {session_id} audio task failed: {err}");
            PipelineEvent::Failed {
                session_id: Some(session_id),
                message: format!("audio task failed: {err}"),
            }
            .emit(&app);
            return;
        }
    };

    // --- Selection leg result (Transform only). Unlike the screenshot, this
    // is *not* degradable: with nothing to transform there is no safe output
    // to inject, so a failed read fails the session — here, before an STT
    // round trip is spent on an instruction with no subject.
    let selection = match selection {
        Some(Ok(Ok(text))) => Some(text),
        Some(Ok(Err(err))) => {
            log::warn!("session {session_id} selection read failed: {err}");
            PipelineEvent::Failed {
                session_id: Some(session_id),
                message: err.to_string(),
            }
            .emit(&app);
            return;
        }
        Some(Err(err)) => {
            // Join error: the blocking task panicked or was aborted.
            log::error!("session {session_id} selection task failed: {err}");
            PipelineEvent::Failed {
                session_id: Some(session_id),
                message: format!("selection task failed: {err}"),
            }
            .emit(&app);
            return;
        }
        None => None,
    };

    if let Some(frame) = frame.as_ref() {
        PipelineEvent::CaptureDone {
            session_id,
            width: frame.width,
            height: frame.height,
            source_width: frame.source_width,
            source_height: frame.source_height,
            monitor: frame.monitor.clone(),
            elapsed_ms: frame.elapsed_ms,
        }
        .emit(&app);
    }

    // --- Speech to text. Deliberately *after* the supersede check above: a
    // stale session must never reach the network.
    //
    // One short read of settings, cloned out — the guard must not cross the
    // `.await` below. The dictionary rides along as Whisper prompt biasing.
    let snapshot = settings.read().clone();

    // WAV encoding is the same class of work as resampling — pure CPU over up
    // to a few million samples — so it goes to the blocking pool, not an async
    // worker. The samples move with it; everything else we still need is Copy.
    let sample_count = captured.samples.len();
    let sample_rate = captured.sample_rate;
    let samples = std::mem::take(&mut captured.samples);
    let wav = match async_runtime::spawn_blocking(move || encode_wav(&samples, sample_rate)).await
    {
        Ok(Ok(wav)) => wav,
        Ok(Err(err)) => {
            log::warn!("session {session_id} wav encode failed: {err}");
            PipelineEvent::Failed {
                session_id: Some(session_id),
                message: err.to_string(),
            }
            .emit(&app);
            return;
        }
        Err(err) => {
            // Join error: the blocking task panicked or was aborted.
            log::error!("session {session_id} wav encode task failed: {err}");
            PipelineEvent::Failed {
                session_id: Some(session_id),
                message: format!("wav encode task failed: {err}"),
            }
            .emit(&app);
            return;
        }
    };

    // `Active::drop` cancels this token too, but that only fires when this task
    // ends — the supervisor's clone (see `detach_finalize`) is what lets
    // `Alt+Escape` abort the request while it is still on the wire.
    let transcript = tokio::select! {
        _ = active.cancel.cancelled() => {
            log::info!("session {session_id} cancelled during transcription");
            PipelineEvent::Cancelled { session_id }.emit(&app);
            return;
        }
        result = stt::transcribe(&app, &snapshot, wav, &snapshot.dictionary) => result,
    };

    let transcript = match transcript {
        Ok(transcript) => transcript,
        Err(err) => {
            log::warn!("session {session_id} transcription failed: {err}");
            PipelineEvent::Failed {
                session_id: Some(session_id),
                message: err.to_string(),
            }
            .emit(&app);
            return;
        }
    };

    // Text in hand before `Ready`, so the UI shows it while cleanup runs.
    let stt_ms = transcript.elapsed_ms;
    let raw = transcript.text;
    PipelineEvent::Transcribed {
        session_id,
        text: raw.clone(),
    }
    .emit(&app);

    // --- Cleanup. Snippet expansion happens in Rust first, then the model
    // call; `CleanupProvider::None` returns the transcript untouched. Raced
    // against the cancel token exactly like STT above.
    let cleaned = tokio::select! {
        _ = active.cancel.cancelled() => {
            log::info!("session {session_id} cancelled during cleanup");
            PipelineEvent::Cancelled { session_id }.emit(&app);
            return;
        }
        result = llm::client::cleanup(&app, &snapshot, &raw) => result,
    };

    // A failed cleanup must never lose the transcript: degrade to the raw text
    // and log, exactly as `flatten_capture` degrades a failed screenshot.
    // Deliberately *not* a `Failed` event — the dictation itself succeeded.
    // For Transform this text is the *instruction*, still to be applied to the
    // selection below; filler stripping helps there too ("um, make this, like,
    // formal" is a better instruction cleaned).
    let mut text = match cleaned {
        Ok(text) => text,
        Err(err) => {
            log::warn!("session {session_id} cleanup failed, keeping the raw transcript: {err}");
            raw
        }
    };

    // --- Injection. Only Dictate delivers by typing into the app that had
    // focus at key-down (Bridge and Transform deliver elsewhere); the text is
    // final here, after the last cancel-select above.
    //
    // The token is checked once up front rather than raced: injection is tens
    // of milliseconds, and the work runs in `spawn_blocking`, which cannot be
    // interrupted anyway (notes §11) — a select would cancel nothing, only
    // pretend to.
    if intent.injects_text() {
        if active.cancel.is_cancelled() {
            log::info!("session {session_id} cancelled before injection");
            PipelineEvent::Cancelled { session_id }.emit(&app);
            return;
        }
        if let Err(err) = inject::inject_text(text.clone()).await {
            // Injecting *is* the point of Dictate, so unlike cleanup this
            // failure is a session failure, not a degradation.
            log::warn!("session {session_id} injection failed: {err}");
            PipelineEvent::Failed {
                session_id: Some(session_id),
                message: err.to_string(),
            }
            .emit(&app);
            return;
        }
    }

    // --- Transform. The selection was read at key-down; the cleaned transcript
    // is the instruction. Unlike cleanup there is **no degrade-to-raw
    // fallback**: the result is typed over the user's live selection, and
    // injecting the wrong thing there is destructive — a transform error fails
    // the session outright.
    if intent == Intent::Transform {
        if active.cancel.is_cancelled() {
            log::info!("session {session_id} cancelled before transform");
            PipelineEvent::Cancelled { session_id }.emit(&app);
            return;
        }
        let Some(selection) = selection else {
            // Unreachable — the selection leg is armed for every Transform
            // session and its failures return above — but never transform
            // nothing.
            log::error!("session {session_id} transform without a selection");
            PipelineEvent::Failed {
                session_id: Some(session_id),
                message: "internal error: transform without a selection".into(),
            }
            .emit(&app);
            return;
        };
        let transformed = tokio::select! {
            _ = active.cancel.cancelled() => {
                log::info!("session {session_id} cancelled during transform");
                PipelineEvent::Cancelled { session_id }.emit(&app);
                return;
            }
            result = llm::client::transform(&app, &snapshot, &text, &selection) => result,
        };
        text = match transformed {
            Ok(text) => text,
            Err(err) => {
                log::warn!("session {session_id} transform failed: {err}");
                PipelineEvent::Failed {
                    session_id: Some(session_id),
                    message: err.to_string(),
                }
                .emit(&app);
                return;
            }
        };
        // The selection is still highlighted only if focus never left the
        // user's app, so typing replaces it in place — the same injection path
        // as Dictate, with the same up-front cancel check (a blocking
        // injection cannot be interrupted mid-flight; notes §11).
        if active.cancel.is_cancelled() {
            log::info!("session {session_id} cancelled before transform injection");
            PipelineEvent::Cancelled { session_id }.emit(&app);
            return;
        }
        if let Err(err) = inject::inject_text(text.clone()).await {
            log::warn!("session {session_id} transform injection failed: {err}");
            PipelineEvent::Failed {
                session_id: Some(session_id),
                message: err.to_string(),
            }
            .emit(&app);
            return;
        }
    }

    // Screenshot facts the tail of this function still needs once `frame` has
    // been moved into the bridge below.
    let has_image = frame.is_some();
    let capture_ms = frame.as_ref().map_or(0, |f| f.elapsed_ms);

    // --- Bridge. Only Bridge delivers to a web LLM: open/focus the target,
    // wait for the tab, paste the screenshot, type the question. Bridging *is*
    // the point of the intent, so a failure here is a `Failed`, not a
    // degradation.
    //
    // Unlike injection, this stage is raced against the cancel token: the
    // multi-second focus wait inside `deliver` is an async sleep, so a select
    // genuinely can interrupt it. (The final paste/type `spawn_blocking` still
    // runs to completion — notes §11.) The up-front check covers the common
    // case of a cancel that landed during cleanup.
    if intent == Intent::Bridge {
        if active.cancel.is_cancelled() {
            log::info!("session {session_id} cancelled before bridging");
            PipelineEvent::Cancelled { session_id }.emit(&app);
            return;
        }
        let url = snapshot.providers.web_target.url().to_string();
        let delay = Duration::from_millis(snapshot.behavior.bridge_delay_ms);
        let bridged = tokio::select! {
            _ = active.cancel.cancelled() => {
                log::info!("session {session_id} cancelled during bridging");
                PipelineEvent::Cancelled { session_id }.emit(&app);
                return;
            }
            result = bridge::deliver(url, delay, frame, text.clone()) => result,
        };
        if let Err(err) = bridged {
            log::warn!("session {session_id} bridge failed: {err}");
            PipelineEvent::Failed {
                session_id: Some(session_id),
                message: err.to_string(),
            }
            .emit(&app);
            return;
        }
    }

    let total_ms = started.elapsed().as_millis() as u64;
    let timings = Timings {
        capture_ms,
        record_ms,
        stt_ms,
        finalize_ms: total_ms.saturating_sub(record_ms),
        total_ms,
    };

    log::info!(
        "session {session_id} ({}) ready: {} ms audio, image: {}, stt {} ms, finalize {} ms",
        intent.as_str(),
        captured.duration_ms,
        has_image,
        timings.stt_ms,
        timings.finalize_ms
    );

    // `Ready` still carries the final text for the UI; for Dictate and
    // Transform it has also just been typed into the target app.
    PipelineEvent::Ready {
        session_id,
        intent,
        text,
        duration_ms: captured.duration_ms,
        sample_count,
        peak: captured.peak,
        has_image,
        truncated: captured.truncated,
        timings,
    }
    .emit(&app);

    PipelineEvent::Idle.emit(&app);
}

/// A failed screenshot must not fail the whole session — the transcript is still
/// useful, and telling the user "no screenshot" beats losing what they said.
fn flatten_capture(
    session_id: u64,
    result: std::result::Result<Result<CaptureFrame>, tauri::Error>,
) -> Option<CaptureFrame> {
    match result {
        Ok(Ok(frame)) => Some(frame),
        Ok(Err(err)) => {
            log::error!("session {session_id} screen capture failed: {err}");
            None
        }
        Err(err) => {
            log::error!("session {session_id} capture task failed: {err}");
            None
        }
    }
}

/// Periodic housekeeping: progress for the UI, and the hard duration cap.
fn tick<R: Runtime>(
    app: &AppHandle<R>,
    audio: &Arc<AudioEngine>,
    settings: &Arc<RwLock<Settings>>,
    active: &mut Option<Active>,
    finalizing: &mut Option<CancellationToken>,
) {
    let Some(current) = active.as_ref() else {
        return;
    };

    let elapsed_ms = current.elapsed_ms();
    let max_ms = settings.read().audio.max_utterance_secs as u64 * 1_000;

    if elapsed_ms >= max_ms {
        // Stop rather than discard: two minutes of speech is worth keeping even
        // though the user forgot to close the session.
        log::warn!("session {} hit the duration cap; stopping", current.id);
        let current = active.take().expect("checked above");
        detach_finalize(app, audio, settings, current, finalizing);
        return;
    }

    PipelineEvent::Recording {
        session_id: current.id,
        elapsed_ms,
        level: audio.input_level(),
    }
    .emit(app);
}
