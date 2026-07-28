//! The audio engine: a dedicated OS thread that owns the microphone.
//!
//! # Why a thread and not an async task
//!
//! `cpal::Stream` is **not `Send` on every platform** (CoreAudio in particular),
//! and dropping it runs blocking teardown against the OS audio subsystem. Both
//! facts make it unsafe to hold across an `.await`. So the stream is created on
//! this thread, lives in a local variable here, and never crosses a thread
//! boundary. Everything else talks to it through:
//!
//! * an `mpsc::Sender<Cmd>` for device lifecycle (open / close), and
//! * an `Arc<Mutex<CaptureBuffer>>` for the sample data.
//!
//! Session start/stop therefore never touches the device at all — it just
//! mutates the shared buffer. That is what makes `start_session` ~microseconds
//! instead of the 20–200 ms a WASAPI/CoreAudio device open costs.
//!
//! # Pre-warming
//!
//! With `prewarm` on, the stream is opened at app start and left running, its
//! samples flowing into a short pre-roll ring that is continuously overwritten.
//! On hotkey we flip a flag and the ring becomes the head of the recording.
//! Net effect: capture latency is effectively zero *and* we recover the audio
//! from just before the keypress.
//!
//! The trade-off is real and belongs to the user, not to us: the OS mic
//! indicator stays lit while pre-warmed. `Settings::audio.prewarm` controls it,
//! and with it off we open the device on demand and lose the pre-roll.
//!
//! # Device selection
//!
//! `Settings::audio.device_name` pins the input device by its OS-reported name
//! (what [`list_input_devices`] returns); empty means the OS default. A
//! configured name that no longer matches any device — unplugged, renamed —
//! falls back to the default with a warning rather than failing, because a mic
//! that silently never opens is far harder to diagnose than one that records
//! from the wrong source.
//!
//! The name is read once, when the engine thread spawns, and applies whenever
//! the stream is next opened. There is deliberately no hot device-swap: the
//! stream is `!Send` and owned by this thread for the process lifetime
//! (invariant #4), so changing devices means restarting the app.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use parking_lot::Mutex;

use crate::audio::buffer::CaptureBuffer;
use crate::audio::resample::{self, TARGET_SAMPLE_RATE};
use crate::error::{AppError, Result};

/// The device's negotiated format, discovered when the stream opens.
#[derive(Debug, Clone, Copy)]
pub struct DeviceFormat {
    pub sample_rate: u32,
    pub channels: usize,
}

/// A finished recording, resampled and ready for STT.
#[derive(Debug, Clone)]
pub struct CapturedAudio {
    pub session_id: u64,
    /// Mono f32 at [`TARGET_SAMPLE_RATE`].
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub duration_ms: u64,
    pub peak: f32,
    pub rms: f32,
    /// The recording hit the configured maximum length and was truncated.
    pub truncated: bool,
}

enum Cmd {
    Open,
    Close,
    Shutdown,
}

pub struct AudioEngine {
    /// Wrapped in a mutex purely to guarantee `Sync`.
    ///
    /// `AudioEngine` is stored in Tauri's managed state and shared across tasks,
    /// which requires `Send + Sync`. `std::sync::mpsc::Sender` is `Send` but its
    /// `Sync` status has varied across std versions, so rather than depend on
    /// that, the mutex makes it unconditional. It costs nothing: the only
    /// messages are device open/close, which happen a handful of times per run.
    cmd: Mutex<Sender<Cmd>>,
    buffer: Arc<Mutex<CaptureBuffer>>,
    format: Arc<Mutex<Option<DeviceFormat>>>,
    stream_open: Arc<AtomicBool>,
    last_error: Arc<Mutex<Option<String>>>,
    preroll_ms: u32,
}

impl AudioEngine {
    /// Spawns the engine thread. Does not open the device — call
    /// [`AudioEngine::prewarm`] for that.
    ///
    /// `device_name` is the configured input device (empty = OS default); it is
    /// captured here and used for every subsequent open.
    pub fn spawn(preroll_ms: u32, device_name: String) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();

        let buffer = Arc::new(Mutex::new(CaptureBuffer::new()));
        let format: Arc<Mutex<Option<DeviceFormat>>> = Arc::new(Mutex::new(None));
        let stream_open = Arc::new(AtomicBool::new(false));
        let last_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        let engine = Self {
            cmd: Mutex::new(cmd_tx),
            buffer: Arc::clone(&buffer),
            format: Arc::clone(&format),
            stream_open: Arc::clone(&stream_open),
            last_error: Arc::clone(&last_error),
            preroll_ms,
        };

        thread::Builder::new()
            .name("whisper-screen-audio".into())
            .spawn(move || {
                // `stream` never leaves this scope. That is the whole point.
                let mut stream: Option<cpal::Stream> = None;

                while let Ok(cmd) = cmd_rx.recv() {
                    match cmd {
                        Cmd::Open => {
                            if stream.is_some() {
                                continue;
                            }
                            match open_input_stream(&buffer, &format, preroll_ms, &device_name) {
                                Ok(new_stream) => match new_stream.play() {
                                    Ok(()) => {
                                        stream = Some(new_stream);
                                        stream_open.store(true, Ordering::Release);
                                        *last_error.lock() = None;
                                        log::info!("audio input stream open: {:?}", *format.lock());
                                    }
                                    Err(err) => {
                                        let msg = format!("failed to start stream: {err}");
                                        log::error!("{msg}");
                                        *last_error.lock() = Some(msg);
                                    }
                                },
                                Err(err) => {
                                    log::error!("failed to open input device: {err}");
                                    *last_error.lock() = Some(err.to_string());
                                }
                            }
                        }
                        Cmd::Close => {
                            // Dropping the stream is the documented way to close
                            // the device; it blocks briefly, which is fine here
                            // because this thread is not the async runtime.
                            stream = None;
                            stream_open.store(false, Ordering::Release);
                            log::info!("audio input stream closed");
                        }
                        Cmd::Shutdown => break,
                    }
                }

                drop(stream);
                log::info!("audio engine thread exiting");
            })
            .expect("failed to spawn audio engine thread");

        engine
    }

    /// Sends a command to the engine thread. A closed channel means the thread
    /// died, which is unrecoverable for audio but must not panic the app.
    fn command(&self, cmd: Cmd) -> Result<()> {
        self.cmd
            .lock()
            .send(cmd)
            .map_err(|_| AppError::AudioEngineDown)
    }

    /// Opens the device and starts filling the pre-roll ring.
    pub fn prewarm(&self) -> Result<()> {
        self.command(Cmd::Open)
    }

    /// Releases the device (turns off the OS mic indicator).
    pub fn release(&self) -> Result<()> {
        self.command(Cmd::Close)
    }

    pub fn is_open(&self) -> bool {
        self.stream_open.load(Ordering::Acquire)
    }

    pub fn format(&self) -> Option<DeviceFormat> {
        *self.format.lock()
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().clone()
    }

    /// Begins recording into session `id`. Cheap enough to call from a hotkey
    /// handler: it takes one uncontended mutex and, if the device is already
    /// pre-warmed, does no I/O whatsoever.
    ///
    /// `use_preroll` is ignored when the stream was not already open, because
    /// there is nothing in the ring yet.
    pub fn start_session(&self, id: u64, max_utterance_secs: u32, use_preroll: bool) -> Result<()> {
        let was_open = self.is_open();

        // Size the session buffer in *device*-rate samples, since that is what
        // the callback writes. Fall back to a 48 kHz assumption if the device
        // has not reported its format yet.
        let rate = self.format().map_or(48_000, |f| f.sample_rate);
        let max_samples = rate as usize * max_utterance_secs as usize;

        self.buffer
            .lock()
            .begin(id, max_samples, use_preroll && was_open);

        if !was_open {
            // On-demand open: the first ~50 ms of speech is lost. This is the
            // cost of `prewarm: false`, and it is why pre-warm is the default.
            self.command(Cmd::Open)?;
        }
        Ok(())
    }

    /// Closes session `id`, resamples to 16 kHz mono and returns it.
    ///
    /// **Call this from `spawn_blocking`.** Resampling a 30-second utterance is
    /// a few milliseconds of pure CPU — small, but not something to run on an
    /// async worker that other tasks are waiting on.
    ///
    /// Returns [`AppError::Superseded`] if `id` is no longer the live session,
    /// which is how a cancelled or replaced session gets dropped safely.
    pub fn finish_session(&self, id: u64) -> Result<CapturedAudio> {
        let raw = {
            // Scope the lock: never hold it across the resample below.
            let mut buffer = self.buffer.lock();
            buffer.finish(id).ok_or(AppError::Superseded(id))?
        };

        if raw.samples.is_empty() {
            return Err(AppError::EmptyRecording);
        }

        let device_rate = self.format().map_or(48_000, |f| f.sample_rate);
        let samples = resample::to_target_rate(&raw.samples, device_rate);
        if samples.is_empty() {
            return Err(AppError::EmptyRecording);
        }

        let duration_ms = (samples.len() as u64 * 1000) / TARGET_SAMPLE_RATE as u64;

        Ok(CapturedAudio {
            session_id: id,
            peak: resample::peak(&samples),
            rms: resample::rms(&samples),
            duration_ms,
            truncated: raw.overflowed,
            sample_rate: TARGET_SAMPLE_RATE,
            samples,
        })
    }

    /// Throws away session `id` without producing audio.
    pub fn discard_session(&self, id: u64) {
        self.buffer.lock().discard(id);
    }

    /// Samples buffered so far, in device-rate frames — drives the level meter
    /// without copying the audio out.
    pub fn session_len(&self) -> usize {
        self.buffer.lock().session_len()
    }

    pub fn active_session(&self) -> Option<u64> {
        self.buffer.lock().active_id()
    }

    /// RMS of roughly the last 100 ms of the live session, in 0.0..=1.0.
    ///
    /// Returns 0.0 when nothing is recording, which is exactly what the meter
    /// should show.
    pub fn input_level(&self) -> f32 {
        let window = self.format().map_or(4_800, |f| (f.sample_rate / 10) as usize);
        self.buffer.lock().recent_rms(window)
    }

    pub fn preroll_ms(&self) -> u32 {
        self.preroll_ms
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        let _ = self.command(Cmd::Shutdown);
    }
}

/// The OS-reported name of a device — what `audio.device_name` is matched
/// against and what the settings UI lists.
fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "<unknown>".into())
}

/// Names of every input device the OS reports, for the settings UI.
///
/// **Call this from `spawn_blocking`** — cpal enumerates the OS audio graph
/// synchronously. The `list_input_devices` command does.
pub fn list_input_devices() -> Result<Vec<String>> {
    let host = cpal::default_host();
    let devices = host
        .input_devices()
        .map_err(|err| AppError::Audio(format!("could not list input devices: {err}")))?;
    Ok(devices.map(|device| device_name(&device)).collect())
}

/// The device-name decision, factored pure so it is testable: returns the
/// available name to use, or `None` for "fall back to the OS default" — which
/// is the answer for both an empty configured name and a configured name that
/// matches nothing (device unplugged or renamed).
fn choose_device_name<'a>(configured: &str, available: &'a [String]) -> Option<&'a str> {
    let configured = configured.trim();
    if configured.is_empty() {
        return None;
    }
    available
        .iter()
        .find(|name| name.as_str() == configured)
        .map(String::as_str)
}

/// Picks the input device: the configured name if a connected device matches it
/// exactly, otherwise the OS default with a warning.
fn input_device(host: &cpal::Host, configured: &str) -> Result<cpal::Device> {
    if !configured.trim().is_empty() {
        match host.input_devices() {
            Ok(devices) => {
                let available: Vec<(String, cpal::Device)> = devices
                    .map(|device| (device_name(&device), device))
                    .collect();
                let names: Vec<String> =
                    available.iter().map(|(name, _)| name.clone()).collect();
                match choose_device_name(configured, &names) {
                    Some(chosen) => {
                        let index = names
                            .iter()
                            .position(|name| name == chosen)
                            .expect("the chosen name came from this list");
                        return Ok(available
                            .into_iter()
                            .nth(index)
                            .expect("the index came from this list")
                            .1);
                    }
                    None => log::warn!(
                        "configured input device {:?} is not connected; falling back to the OS default",
                        configured.trim()
                    ),
                }
            }
            Err(err) => {
                log::warn!("could not list input devices ({err}); falling back to the OS default")
            }
        }
    }
    host.default_input_device().ok_or(AppError::NoInputDevice)
}

/// Opens the configured (or default) input device and builds a running stream.
///
/// Runs on the engine thread only.
fn open_input_stream(
    buffer: &Arc<Mutex<CaptureBuffer>>,
    format: &Arc<Mutex<Option<DeviceFormat>>>,
    preroll_ms: u32,
    configured: &str,
) -> Result<cpal::Stream> {
    let host = cpal::default_host();
    let device = input_device(&host, configured)?;

    let supported = device
        .default_input_config()
        .map_err(|err| AppError::Audio(format!("no default input config: {err}")))?;

    let sample_format = supported.sample_format();
    let config: StreamConfig = supported.config();
    let channels = config.channels as usize;
    // cpal 0.18 models `SampleRate` as a plain `u32`.
    let sample_rate: u32 = config.sample_rate;

    if channels == 0 {
        return Err(AppError::Audio("device reported 0 channels".into()));
    }

    log::info!(
        "input device {:?}: {} ch @ {} Hz, {:?}",
        device_name(&device),
        channels,
        sample_rate,
        sample_format
    );

    *format.lock() = Some(DeviceFormat {
        sample_rate,
        channels,
    });

    // Pre-roll is sized in device-rate samples, mono.
    let preroll_samples = (sample_rate as u64 * preroll_ms as u64 / 1000) as usize;
    buffer.lock().configure_preroll(preroll_samples);

    // Sample conversion is written out by hand rather than going through
    // cpal's `FromSample` machinery: it is trivially correct, allocation-free,
    // and immune to conversion-trait churn between cpal releases.
    match sample_format {
        SampleFormat::F32 => build_stream::<f32>(&device, config, channels, buffer, |s| s),
        SampleFormat::I16 => {
            build_stream::<i16>(&device, config, channels, buffer, |s| s as f32 / 32_768.0)
        }
        SampleFormat::U16 => build_stream::<u16>(&device, config, channels, buffer, |s| {
            (s as f32 - 32_768.0) / 32_768.0
        }),
        SampleFormat::I32 => build_stream::<i32>(&device, config, channels, buffer, |s| {
            s as f32 / 2_147_483_648.0
        }),
        SampleFormat::I8 => {
            build_stream::<i8>(&device, config, channels, buffer, |s| s as f32 / 128.0)
        }
        SampleFormat::U8 => build_stream::<u8>(&device, config, channels, buffer, |s| {
            (s as f32 - 128.0) / 128.0
        }),
        SampleFormat::F64 => build_stream::<f64>(&device, config, channels, buffer, |s| s as f32),
        other => Err(AppError::Audio(format!(
            "unsupported input sample format: {other:?}"
        ))),
    }
}

/// Builds the input stream for one concrete sample type.
///
/// The callback is the only realtime-critical code in the app. It does exactly
/// two things per frame — convert and average — then one bounded push. No
/// allocation, no syscalls, no logging.
fn build_stream<T>(
    device: &cpal::Device,
    config: StreamConfig,
    channels: usize,
    buffer: &Arc<Mutex<CaptureBuffer>>,
    to_f32: fn(T) -> f32,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample + Copy + Send + 'static,
{
    let buffer = Arc::clone(buffer);
    let inv_channels = 1.0 / channels as f32;

    device
        .build_input_stream::<T, _, _>(
            config,
            move |data: &[T], _info| {
                let mut buf = buffer.lock();
                for frame in data.chunks_exact(channels) {
                    let mut sum = 0.0f32;
                    for sample in frame {
                        sum += to_f32(*sample);
                    }
                    buf.push(sum * inv_channels);
                }
            },
            |err| log::error!("audio stream error: {err}"),
            None,
        )
        .map_err(|err| AppError::Audio(format!("failed to build input stream: {err}")))
}


#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        vec![
            "Microphone (USB Audio Device)".into(),
            "Headset (Wireless Controller)".into(),
        ]
    }

    #[test]
    fn an_empty_configured_name_means_os_default() {
        assert_eq!(choose_device_name("", &names()), None);
        assert_eq!(choose_device_name("   ", &names()), None);
    }

    #[test]
    fn an_exact_match_is_used() {
        assert_eq!(
            choose_device_name("Headset (Wireless Controller)", &names()),
            Some("Headset (Wireless Controller)")
        );
    }

    #[test]
    fn a_missing_device_falls_back_to_the_default() {
        // Unplugged or renamed devices must not fail — the default mic is
        // better than no mic.
        assert_eq!(choose_device_name("Microphone (unplugged)", &names()), None);
    }

    #[test]
    fn matching_is_exact_not_substring() {
        assert_eq!(choose_device_name("Microphone", &names()), None);
        assert_eq!(choose_device_name("microphone (usb audio device)", &names()), None);
    }
}
