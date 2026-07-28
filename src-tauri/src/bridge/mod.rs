//! The web bridge: landing the screenshot and the spoken question in a web LLM.
//!
//! This is the payoff of `Intent::Bridge` — the user holds the bridge hotkey,
//! asks "what's wrong with this error", and the screenshot they were already
//! looking at plus the cleaned question appear in a Claude/ChatGPT/Gemini
//! conversation, ready to send.
//!
//! # The fragile part is timing, not the clipboard
//!
//! The sequence is: open/focus the target URL → wait → put the screenshot on
//! the clipboard → paste → type the question. Every step except one is
//! deterministic. The exception is the wait: **there is no reliable signal
//! that a browser tab has finished loading and focused its input.** The OS can
//! tell us the browser process launched; it cannot tell us the page rendered
//! and its text field will accept a paste. Too short and the paste lands in
//! whatever had focus before — the worst outcome, a screenshot typed into the
//! user's Slack. So the delay is a *setting* (`behavior.bridge_delay_ms`,
//! default 2.5 s), tuned by the user to their machine and browser, not a magic
//! constant we pretend is universal.
//!
//! # Why the clipboard path never PNG-encodes
//!
//! `capture::CaptureFrame` deliberately holds raw RGBA8 and defers PNG
//! encoding. This module is why: `arboard::ImageData` takes exactly the same
//! layout — tightly packed RGBA8, 4 bytes per pixel — so the hand-off is two
//! `usize` casts and a borrowed slice. Encoding to PNG here would spend ~20 ms
//! producing bytes the clipboard never wants. Only an HTTP upload path (which
//! does not exist yet) would need `frame.to_png()`.
//!
//! # Clipboard discipline
//!
//! The bridge *replaces* the user's clipboard with an image. That is the same
//! hazard `inject` was built around, so it reuses
//! [`inject::clipboard::SavedClipboard`]: the previous contents are captured
//! first and restored in `Drop`, after the paste has had its settle beat — on
//! every path, including the error ones.
//!
//! # Submit is the user's call
//!
//! The bridge pastes the image and types the question, then stops. It does
//! **not** synthesize Enter: an auto-submitted prompt with a mis-pasted image
//! or a misheard question spends the user's tokens and their dignity in a
//! shared chat. Leaving the composed message in the input box costs one
//! keystroke and buys a review step. If an auto-submit toggle is ever wanted,
//! it belongs in `settings.behavior`, not hardcoded here.
//!
//! # Everything blocks
//!
//! The opener, `arboard`, and `enigo` all talk to the OS synchronously, so per
//! invariant #6 they run inside `spawn_blocking`. The long focus wait is the
//! exception: it is a `tokio::time::sleep` in async land — sleeping for
//! seconds on a blocking-pool thread would squat a pool thread, and an async
//! sleep is what lets the caller's cancel `select!` actually interrupt the
//! wait. (`spawn_blocking` itself cannot be interrupted; the final paste/type
//! section is tens of milliseconds and runs to completion regardless — the
//! acknowledged limitation of notes §11.)

use std::borrow::Cow;
use std::time::Duration;

use arboard::ImageData;

use crate::capture::CaptureFrame;
use crate::error::{AppError, Result};
use crate::inject::{self, clipboard::SavedClipboard, keyboard};

/// Delivers `frame` and `text` to the web LLM at `url`.
///
/// The async entry point the pipeline calls. `frame` is `None` when the
/// screenshot leg failed — the question still gets delivered text-only, on
/// the same principle as `inject`: the text landing beats a clean error
/// explaining why it didn't. `delay` is the focus wait described in the
/// module doc; the caller reads it from settings.
pub async fn deliver(
    url: String,
    delay: Duration,
    frame: Option<CaptureFrame>,
    text: String,
) -> Result<()> {
    // Opening the URL is a fast ShellExecute on Windows / `open` on macOS, but
    // it is still a synchronous OS call, so it goes to the blocking pool like
    // everything else in this module.
    tauri::async_runtime::spawn_blocking(move || open_target(&url))
        .await
        .map_err(|err| AppError::Bridge(format!("open task failed: {err}")))??;

    // The fragile wait — async, so the pipeline's cancel token can interrupt
    // it (a `select!` can drop this sleep; it could not drop a blocking one).
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }

    tauri::async_runtime::spawn_blocking(move || paste_and_type(frame.as_ref(), &text))
        .await
        .map_err(|err| AppError::Bridge(format!("paste task failed: {err}")))?
}

/// Opens or focuses the target in the user's default browser.
///
/// Uses the plugin's free function rather than `AppHandle::opener()` so the
/// bridge does not need to drag an app handle onto a blocking thread. This is
/// a direct Rust call, not an IPC command, so no capability entry is involved
/// (capabilities gate the webview→plugin boundary, not Rust→plugin).
fn open_target(url: &str) -> Result<()> {
    tauri_plugin_opener::open_url(url, None::<&str>)
        .map_err(|err| AppError::Bridge(format!("could not open {url}: {err}")))
}

/// The blocking body of [`deliver`]: save → set image → paste → settle →
/// restore (on drop) → type the question.
fn paste_and_type(frame: Option<&CaptureFrame>, text: &str) -> Result<()> {
    if let Some(frame) = frame {
        let mut saved = SavedClipboard::capture()?;
        saved.set_image(image_data(frame))?;
        keyboard::paste()?;
        // Give the browser a beat to read the clipboard before the guard
        // restores the user's previous contents — the same settle the text
        // injection path applies (`inject::PASTE_SETTLE`).
        std::thread::sleep(inject::PASTE_SETTLE);
        // `saved` drops here, restoring the user's clipboard. The browser has
        // already consumed the image; typing below does not touch the
        // clipboard, so the natural end-of-scope drop is correctly placed.
    }

    if !text.is_empty() {
        keyboard::type_text(text)?;
    }
    Ok(())
}

/// The RGBA hand-off. Both sides are RGBA8 at 4 bytes per pixel, so the only
/// work is the `usize` casts; the bytes are *borrowed*, not copied (a 1568 px
/// frame is ~10 MB). Pure and factored out so the cast discipline is
/// unit-tested.
fn image_data(frame: &CaptureFrame) -> ImageData<'_> {
    ImageData {
        width: frame.width as usize,
        height: frame.height as usize,
        bytes: Cow::Borrowed(&frame.rgba),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32, height: u32) -> CaptureFrame {
        CaptureFrame {
            width,
            height,
            rgba: vec![7; (width * height * 4) as usize],
            source_width: width,
            source_height: height,
            monitor: "test".into(),
            elapsed_ms: 0,
        }
    }

    #[test]
    fn image_data_is_a_direct_borrow_of_the_frame() {
        let frame = frame(3, 2);
        let img = image_data(&frame);
        assert_eq!(img.width, 3);
        assert_eq!(img.height, 2);
        assert_eq!(img.bytes.len(), frame.rgba.len());
        // Borrowed, not copied: no 10 MB clone on the way to the clipboard.
        assert!(matches!(img.bytes, Cow::Borrowed(_)));
    }

    #[test]
    fn image_data_dimensions_are_usize_casts_not_scaling() {
        let frame = frame(1568, 882);
        let img = image_data(&frame);
        assert_eq!((img.width, img.height), (1568_usize, 882_usize));
    }
}
