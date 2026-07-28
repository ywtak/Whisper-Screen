//! Clipboard save/restore.
//!
//! # Why this module exists
//!
//! The reliable way to inject a long transcript is to put it on the clipboard
//! and send a paste chord. But the clipboard is **the user's data**, not a
//! scratch buffer: silently destroying whatever they had copied is the single
//! worst failure mode of this class of app. So every paste injection goes
//! through [`SavedClipboard`], whose whole job is to guarantee that whatever
//! was on the clipboard before is put back after — even when the paste fails.
//!
//! # What "save" means when there is no text
//!
//! `get_text` returns `Err` on an empty or image-only clipboard. That is
//! normal, not a failure — it just means there is no text to restore, in
//! which case we clear the clipboard on the way out rather than leave our
//! injected text sitting in it. (Restoring a previous *image* is not possible
//! through the text API; the save/restore discipline covers text, which is
//! what dictation workflows carry.)
//!
//! Everything here blocks the calling thread (`arboard` talks to the OS
//! clipboard synchronously), so callers must go through `spawn_blocking` —
//! the async wrapper in [`super`] does (invariant #6).

use arboard::Clipboard;

use crate::error::{AppError, Result};

/// What to do with the clipboard once the injected text has been pasted.
///
/// Pure decision, separated from the OS calls so it is testable: restore the
/// previous text if there was any, otherwise clear so our text does not
/// linger.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RestorePlan {
    PutBack(String),
    Clear,
}

fn restore_plan(previous: &Option<String>) -> RestorePlan {
    match previous {
        Some(text) => RestorePlan::PutBack(text.clone()),
        None => RestorePlan::Clear,
    }
}

/// A clipboard whose previous contents are restored on drop.
///
/// Use it as a scope guard: `capture`, `set_text` the payload, paste, and let
/// the guard fall out of scope. Restoration is in `Drop` precisely so it
/// happens on the error paths too — an early `?` return must not strand the
/// user's clipboard holding our transcript.
pub struct SavedClipboard {
    clipboard: Clipboard,
    previous: Option<String>,
}

impl SavedClipboard {
    /// Opens the clipboard and remembers what it held.
    pub fn capture() -> Result<Self> {
        let mut clipboard = Clipboard::new()
            .map_err(|err| AppError::Clipboard(format!("could not open clipboard: {err}")))?;
        // `Err` on an empty or image-only clipboard is normal — treat as
        // "nothing to put back", not as a failure.
        let previous = clipboard.get_text().ok();
        Ok(Self {
            clipboard,
            previous,
        })
    }

    /// Replaces the clipboard contents with the injection payload.
    pub fn set_text(&mut self, text: &str) -> Result<()> {
        self.clipboard
            .set_text(text)
            .map_err(|err| AppError::Clipboard(format!("could not set clipboard text: {err}")))
    }

    /// What the clipboard held at `capture()` time.
    ///
    /// Selection capture compares against this: a synthetic copy chord that
    /// leaves the clipboard untouched almost always means nothing was
    /// selected, and the read returned the user's own old clipboard.
    pub fn previous_text(&self) -> Option<&str> {
        self.previous.as_deref()
    }

    /// Reads whatever the clipboard holds *now*.
    ///
    /// Selection capture uses this after the copy chord, when the selection
    /// *is* the clipboard. `Err` on an empty or image-only clipboard is
    /// normal, exactly as in `capture` — treat it as "nothing there", not a
    /// failure.
    pub fn get_text(&mut self) -> Option<String> {
        self.clipboard.get_text().ok()
    }

    /// Replaces the clipboard contents with an image payload.
    ///
    /// Used by the web bridge, which pastes a screenshot. The restore-in-Drop
    /// guarantee is unchanged: the previous *text* is still put back after the
    /// paste settles. (A previous *image* cannot be restored through the text
    /// API — the documented limitation in the module doc.)
    pub fn set_image(&mut self, image: arboard::ImageData<'_>) -> Result<()> {
        self.clipboard
            .set_image(image)
            .map_err(|err| AppError::Clipboard(format!("could not set clipboard image: {err}")))
    }
}

impl Drop for SavedClipboard {
    fn drop(&mut self) {
        let result = match restore_plan(&self.previous) {
            RestorePlan::PutBack(text) => self.clipboard.set_text(text),
            RestorePlan::Clear => self.clipboard.clear(),
        };
        // Drop cannot propagate errors — log loudly instead. A failed restore
        // means the user's clipboard still holds our transcript, which is
        // exactly the failure this type exists to prevent.
        if let Err(err) = result {
            log::warn!("could not restore the clipboard after injection: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previous_text_is_put_back() {
        let plan = restore_plan(&Some("the user's spreadsheet formula".to_string()));
        assert_eq!(
            plan,
            RestorePlan::PutBack("the user's spreadsheet formula".to_string())
        );
    }

    #[test]
    fn nothing_saved_means_clear_not_linger() {
        assert_eq!(restore_plan(&None), RestorePlan::Clear);
    }
}
