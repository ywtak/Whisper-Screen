//! The one piece of shared mutable state the realtime audio callback touches.
//!
//! Design constraints, in priority order:
//!
//! 1. **The callback must never allocate and never block for long.** Both the
//!    pre-roll ring and the session vector are pre-allocated to their maximum
//!    size up front, so `push` is a pointer bump in the steady state. The lock
//!    is a `parking_lot::Mutex` held for a few hundred nanoseconds per buffer
//!    (~480 samples); the only other party that takes it is the pipeline, which
//!    also holds it briefly. If you later need hard realtime guarantees, swap
//!    this for an SPSC ring (`ringbuf`) — the public API here won't change.
//!
//! 2. **Memory is bounded.** A session that is never closed stops accumulating
//!    at `max_samples` and sets `overflowed`, rather than growing until OOM.
//!
//! 3. **Pre-roll.** The mic runs continuously while pre-warmed, filling a small
//!    ring. When a session starts, that ring is copied in as the first samples,
//!    so the beginning of the first word — spoken in the ~200ms before the OS
//!    delivers the hotkey — is not clipped. This is why dictation apps feel
//!    like they started listening before you pressed the key.
//!
//! Samples are stored as mono f32 at the *device* sample rate. Downmixing
//! happens in the callback (cheap, and shrinks this buffer by `channels`x);
//! resampling to 16 kHz happens once, off the realtime thread, at session end.

use std::collections::VecDeque;

/// A session that is currently accumulating samples.
struct Session {
    id: u64,
    samples: Vec<f32>,
    max_samples: usize,
    overflowed: bool,
}

pub struct CaptureBuffer {
    preroll: VecDeque<f32>,
    preroll_capacity: usize,
    session: Option<Session>,
}

/// What the pipeline gets back when a session closes.
pub struct RawCapture {
    pub id: u64,
    /// Mono, device sample rate, not yet resampled.
    pub samples: Vec<f32>,
    /// True if the recording hit `max_utterance_secs` and was truncated.
    pub overflowed: bool,
}

impl CaptureBuffer {
    pub fn new() -> Self {
        Self {
            preroll: VecDeque::new(),
            preroll_capacity: 0,
            session: None,
        }
    }

    /// Sizes the pre-roll ring. Called once the device format is known, and
    /// again if the device changes. Pre-allocates so the callback never grows it.
    pub fn configure_preroll(&mut self, samples: usize) {
        self.preroll_capacity = samples;
        self.preroll = VecDeque::with_capacity(samples);
    }

    /// Called from the realtime audio callback. Must stay allocation-free.
    #[inline]
    pub fn push(&mut self, sample: f32) {
        if let Some(session) = self.session.as_mut() {
            if session.samples.len() < session.max_samples {
                session.samples.push(sample);
            } else {
                session.overflowed = true;
            }
        }

        // Keep the ring warm even during a session so a back-to-back session
        // still gets its lead-in.
        if self.preroll_capacity == 0 {
            return;
        }
        if self.preroll.len() == self.preroll_capacity {
            self.preroll.pop_front();
        }
        self.preroll.push_back(sample);
    }

    /// Opens a session, seeding it with whatever is in the pre-roll ring.
    ///
    /// Idempotent per id: starting the same id twice is a no-op, so a duplicate
    /// key-down event (autorepeat) cannot reset a recording in progress.
    pub fn begin(&mut self, id: u64, max_samples: usize, use_preroll: bool) {
        if self.session.as_ref().is_some_and(|s| s.id == id) {
            return;
        }

        let mut samples = Vec::with_capacity(max_samples);
        if use_preroll {
            samples.extend(self.preroll.iter().copied());
        }

        self.session = Some(Session {
            id,
            samples,
            max_samples,
            overflowed: false,
        });
    }

    /// Closes the session and hands the samples to the caller.
    ///
    /// Returns `None` if `id` is not the live session — that is the guard that
    /// makes a late `finish` from a superseded session a no-op instead of
    /// stealing the audio belonging to the session that replaced it.
    pub fn finish(&mut self, id: u64) -> Option<RawCapture> {
        if self.session.as_ref().map(|s| s.id) != Some(id) {
            return None;
        }
        let session = self.session.take()?;
        Some(RawCapture {
            id: session.id,
            samples: session.samples,
            overflowed: session.overflowed,
        })
    }

    /// Drops the session's audio without returning it (user cancelled).
    pub fn discard(&mut self, id: u64) {
        if self.session.as_ref().map(|s| s.id) == Some(id) {
            self.session = None;
        }
    }

    /// Number of samples buffered for the live session, for the level meter.
    pub fn session_len(&self) -> usize {
        self.session.as_ref().map_or(0, |s| s.samples.len())
    }

    pub fn active_id(&self) -> Option<u64> {
        self.session.as_ref().map(|s| s.id)
    }

    /// RMS over the most recent `window` samples of the live session.
    ///
    /// Reads the tail in place instead of copying the audio out, so the level
    /// meter costs the same whether the recording is one second or two minutes.
    pub fn recent_rms(&self, window: usize) -> f32 {
        let Some(session) = self.session.as_ref() else {
            return 0.0;
        };
        let start = session.samples.len().saturating_sub(window);
        let tail = &session.samples[start..];
        if tail.is_empty() {
            return 0.0;
        }
        (tail.iter().map(|s| s * s).sum::<f32>() / tail.len() as f32).sqrt()
    }
}

impl Default for CaptureBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preroll_seeds_the_session() {
        let mut buf = CaptureBuffer::new();
        buf.configure_preroll(4);
        for s in [1.0, 2.0, 3.0, 4.0, 5.0, 6.0] {
            buf.push(s);
        }
        // The ring keeps only the last 4.
        buf.begin(1, 100, true);
        buf.push(7.0);
        let out = buf.finish(1).expect("session 1 is live");
        assert_eq!(out.samples, vec![3.0, 4.0, 5.0, 6.0, 7.0]);
    }

    #[test]
    fn finish_with_a_stale_id_returns_nothing() {
        let mut buf = CaptureBuffer::new();
        buf.begin(1, 100, false);
        buf.push(1.0);
        assert!(buf.finish(2).is_none(), "stale id must not steal audio");
        assert!(buf.finish(1).is_some());
    }

    #[test]
    fn overflow_is_bounded_and_flagged() {
        let mut buf = CaptureBuffer::new();
        buf.begin(1, 3, false);
        for _ in 0..100 {
            buf.push(0.5);
        }
        let out = buf.finish(1).unwrap();
        assert_eq!(out.samples.len(), 3);
        assert!(out.overflowed);
    }

    #[test]
    fn duplicate_begin_does_not_reset_a_live_session() {
        let mut buf = CaptureBuffer::new();
        buf.begin(1, 100, false);
        buf.push(1.0);
        buf.begin(1, 100, false); // key autorepeat
        assert_eq!(buf.session_len(), 1);
    }
}
