//! Silent full-screen capture.
//!
//! Everything here is **blocking**: `xcap` wraps DXGI Desktop Duplication on
//! Windows and `CGDisplayCreateImage` on macOS, both of which are synchronous
//! calls into the compositor. Callers must therefore run this inside
//! `tauri::async_runtime::spawn_blocking` — see `pipeline::run_session`.
//!
//! # Why it doesn't flicker
//!
//! We never show, hide, focus or resize a window as part of capturing. The
//! settings window is a normal hidden window and is not involved. The OS
//! composites the screen as it already is, so there is no visual artefact and,
//! importantly, no change to which application has keyboard focus — which
//! matters because the dictation path is about to type into whatever *was*
//! focused.
//!
//! # Why we downscale immediately
//!
//! A 4K screenshot is 33 MB of RGBA and takes ~300 ms to PNG-encode. Every
//! vision model downsamples to roughly 1.5k on the long edge anyway (Claude's
//! limit is 1568 px), so encoding at native resolution buys nothing but
//! latency. Downscaling first turns PNG encoding into a ~20 ms operation and
//! caps the memory a queued frame can hold.

use std::io::Cursor;
use std::time::Instant;

use image::imageops::FilterType;
use image::ImageFormat;
use xcap::Monitor;

use crate::error::{AppError, Result};

/// A captured screen, downscaled, held as raw RGBA.
///
/// PNG encoding is deliberately *not* done here — the clipboard path wants raw
/// RGBA (that is what `arboard::ImageData` takes) and only the HTTP/upload path
/// wants PNG. Encoding eagerly would spend ~20 ms that the common case throws
/// away.
#[derive(Debug, Clone)]
pub struct CaptureFrame {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA8, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
    /// Pre-downscale dimensions, kept so the UI can show what was captured.
    pub source_width: u32,
    pub source_height: u32,
    pub monitor: String,
    /// Wall-clock time the capture itself took — the number the <50 ms budget
    /// in the PRD refers to. Surfaced to the UI so regressions are visible.
    pub elapsed_ms: u64,
}

impl CaptureFrame {
    /// Encodes to PNG. Call this on a blocking thread, not an async worker.
    pub fn to_png(&self) -> Result<Vec<u8>> {
        let buffer = image::RgbaImage::from_raw(self.width, self.height, self.rgba.clone())
            .ok_or_else(|| {
                AppError::Capture("frame buffer size does not match dimensions".into())
            })?;

        let mut out = Cursor::new(Vec::with_capacity(self.rgba.len() / 4));
        buffer
            .write_to(&mut out, ImageFormat::Png)
            .map_err(|err| AppError::Capture(format!("png encode: {err}")))?;
        Ok(out.into_inner())
    }

    pub fn byte_len(&self) -> usize {
        self.rgba.len()
    }
}

/// Captures the primary monitor, downscaled so its longest edge is at most
/// `max_edge` pixels. Pass `0` to keep native resolution.
pub fn capture_primary(max_edge: u32) -> Result<CaptureFrame> {
    let started = Instant::now();

    let monitors =
        Monitor::all().map_err(|err| AppError::Capture(format!("enumerate monitors: {err}")))?;

    // `is_primary` is fallible per-monitor on some backends; treat an error as
    // "not primary" rather than failing the whole capture, then fall back to
    // the first monitor so a machine with an odd display setup still works.
    let monitor = monitors
        .iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .or_else(|| monitors.first())
        .ok_or(AppError::NoMonitor)?;

    let name = monitor
        .friendly_name()
        .unwrap_or_else(|_| "<unknown display>".to_string());

    let image = monitor
        .capture_image()
        .map_err(|err| AppError::Capture(format!("capture {name}: {err}")))?;

    let source_width = image.width();
    let source_height = image.height();

    let image = match scaled_dimensions(source_width, source_height, max_edge) {
        Some((width, height)) => {
            image::imageops::resize(&image, width, height, FilterType::Triangle)
        }
        None => image,
    };

    Ok(CaptureFrame {
        width: image.width(),
        height: image.height(),
        rgba: image.into_raw(),
        source_width,
        source_height,
        monitor: name,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// Target dimensions that fit inside `max_edge` while preserving aspect ratio,
/// or `None` if the image is already small enough (so we can skip the resize
/// and the allocation it implies).
fn scaled_dimensions(width: u32, height: u32, max_edge: u32) -> Option<(u32, u32)> {
    if max_edge == 0 || width == 0 || height == 0 {
        return None;
    }
    let longest = width.max(height);
    if longest <= max_edge {
        return None;
    }

    let ratio = max_edge as f64 / longest as f64;
    // `max(1)` guards against an extreme aspect ratio rounding a side to zero,
    // which `image` would reject.
    Some((
        ((width as f64 * ratio).round() as u32).max(1),
        ((height as f64 * ratio).round() as u32).max(1),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_resize_when_already_within_budget() {
        assert_eq!(scaled_dimensions(1280, 720, 1568), None);
        assert_eq!(scaled_dimensions(1568, 800, 1568), None);
    }

    #[test]
    fn scales_the_long_edge_to_the_budget() {
        assert_eq!(scaled_dimensions(3840, 2160, 1568), Some((1568, 882)));
        // Portrait: the height is the long edge.
        assert_eq!(scaled_dimensions(2160, 3840, 1568), Some((882, 1568)));
    }

    #[test]
    fn zero_max_edge_means_native_resolution() {
        assert_eq!(scaled_dimensions(3840, 2160, 0), None);
    }

    #[test]
    fn extreme_aspect_ratios_never_round_to_zero() {
        let (w, h) = scaled_dimensions(10_000, 3, 1568).unwrap();
        assert_eq!(w, 1568);
        assert!(h >= 1, "height rounded to {h}");
    }

    #[test]
    fn to_png_rejects_a_mismatched_buffer() {
        let frame = CaptureFrame {
            width: 2,
            height: 2,
            rgba: vec![0; 4], // should be 2*2*4 = 16
            source_width: 2,
            source_height: 2,
            monitor: "test".into(),
            elapsed_ms: 0,
        };
        assert!(frame.to_png().is_err());
    }
}
