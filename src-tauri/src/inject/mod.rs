//! Injection: landing the final text at the user's cursor.
//!
//! # The two ways in
//!
//! * **Type directly** with `enigo.text()` — simple, touches nothing but the
//!   input subsystem, and correct for Unicode and non-US layouts. Its cost
//!   grows with the text, and some fields swallow synthetic text events.
//! * **Clipboard-paste** — save the clipboard, set the transcript, send the
//!   platform paste chord, restore the clipboard. One key chord regardless of
//!   length, and it works in every field that accepts Ctrl+V/Cmd+V — which is
//!   why it is the strategy for anything past a sentence.
//!
//! [`strategy_for`] picks between them on length alone (a pure decision, so
//! it is unit-tested), and [`inject_sync`] falls back to the other strategy
//! when the first one errors — a long transcript that will not paste still
//! deserves a best-effort type-out rather than a silent loss.
//!
//! # The clipboard is the user's data
//!
//! The paste strategy rewrites the system clipboard, and silently destroying
//! what the user had copied is the single worst failure mode of this class
//! of app. [`clipboard::SavedClipboard`] restores the previous contents on
//! drop, so restoration happens even when the paste itself fails.
//!
//! # Everything blocks
//!
//! Both `enigo` and `arboard` talk to the OS synchronously on the calling
//! thread. Per invariant #6, the only public entry point is the async
//! [`inject_text`], which runs the whole sequence inside `spawn_blocking` —
//! an async worker must never make these calls directly.
//!
//! # macOS Accessibility
//!
//! Synthetic input needs the Accessibility grant, which cannot be requested
//! silently (notes §11). No crate we link exposes a verifiable "is trusted"
//! check, so detection is not implemented here — instead every injection
//! error on macOS carries the path to the grant pane (see
//! `keyboard::with_hint`'s use sites), and this paragraph is the obvious
//! home for a real first-run check if a trusted-check API is ever added.

pub mod clipboard;
pub mod keyboard;

use std::time::Duration;

use crate::error::{AppError, Result};

/// Texts at or below this many characters are typed directly; anything
/// longer goes through the clipboard, where the cost is one chord no matter
/// how much was said.
const DIRECT_TYPE_MAX_CHARS: usize = 100;

/// Pause between sending the paste chord and restoring the clipboard.
///
/// A paste chord is a *message*: the target app reads the clipboard when it
/// gets around to processing it, not when we send it. Restoring immediately
/// would race that read and paste the user's old clipboard instead of the
/// transcript. 50 ms is far longer than any same-machine message dispatch
/// and far shorter than a human perceives; this whole module runs on a
/// blocking thread, so the sleep stalls nothing async.
///
/// `pub(crate)` so the bridge (`crate::bridge`) applies the same settle
/// discipline after pasting an image instead of inventing its own constant.
pub(crate) const PASTE_SETTLE: Duration = Duration::from_millis(50);

/// Which way a given text should be injected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// `enigo.text()` straight into the focused field.
    TypeDirect,
    /// Clipboard set + synthetic paste + restore.
    ClipboardPaste,
}

/// Picks the injection strategy. Pure, so the boundary is unit-tested.
///
/// Length is counted in characters, not bytes — the cost we are bounding is
/// per character typed, and a handful of multi-byte emoji is still a handful
/// of characters.
pub fn strategy_for(text: &str) -> Strategy {
    if text.chars().count() <= DIRECT_TYPE_MAX_CHARS {
        Strategy::TypeDirect
    } else {
        Strategy::ClipboardPaste
    }
}

/// Types `text` into whatever application had focus when the hotkey fired.
///
/// The async entry point the pipeline calls: the whole blocking sequence is
/// pushed onto Tokio's blocking pool in one `spawn_blocking`.
pub async fn inject_text(text: String) -> Result<()> {
    tauri::async_runtime::spawn_blocking(move || inject_sync(&text))
        .await
        .map_err(|err| AppError::Inject(format!("injection task failed: {err}")))?
}

/// One injection path, so the primary/fallback pair can be swapped by
/// strategy without duplicating the call-and-fallback logic.
type InjectPath = fn(&str) -> Result<()>;

/// The blocking body of [`inject_text`]. Sync so the strategy choice and the
/// fallback are easy to follow — and so there is exactly one
/// `spawn_blocking` boundary in the module.
fn inject_sync(text: &str) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }

    // Try the chosen strategy, then the other one as a fallback: the text
    // landing somewhere beats a clean error explaining why it didn't.
    let (primary, fallback): (InjectPath, InjectPath) = match strategy_for(text) {
        Strategy::TypeDirect => (keyboard::type_text, paste_via_clipboard),
        Strategy::ClipboardPaste => (paste_via_clipboard, keyboard::type_text),
    };

    primary(text).or_else(|first| {
        log::warn!("primary injection path failed ({first}); trying the fallback");
        fallback(text).map_err(|second| {
            AppError::Inject(format!("typing failed ({first}); paste fallback failed ({second})"))
        })
    })
}

/// The paste strategy: save → set → chord → settle → restore (on drop).
fn paste_via_clipboard(text: &str) -> Result<()> {
    let mut saved = clipboard::SavedClipboard::capture()?;
    saved.set_text(text)?;
    keyboard::paste()?;
    // Give the target app a beat to read the clipboard before the guard
    // restores it — see `PASTE_SETTLE`.
    std::thread::sleep(PASTE_SETTLE);
    Ok(())
    // `saved` drops here, restoring the user's previous clipboard contents —
    // including on every `?` path above.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_typed_directly() {
        assert_eq!(strategy_for("Send this to Sarah."), Strategy::TypeDirect);
        assert_eq!(strategy_for(""), Strategy::TypeDirect);
    }

    #[test]
    fn long_text_goes_through_the_clipboard() {
        let long = "word ".repeat(DIRECT_TYPE_MAX_CHARS);
        assert_eq!(strategy_for(&long), Strategy::ClipboardPaste);
    }

    #[test]
    fn the_threshold_counts_characters_not_bytes() {
        // 100 four-byte characters: over the limit in bytes, at it in chars.
        let emoji = "❤️".repeat(DIRECT_TYPE_MAX_CHARS / 2);
        assert_eq!(emoji.chars().count(), DIRECT_TYPE_MAX_CHARS);
        assert!(emoji.len() > DIRECT_TYPE_MAX_CHARS);
        assert_eq!(strategy_for(&emoji), Strategy::TypeDirect);
    }
}
