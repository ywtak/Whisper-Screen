//! Reading the user's current selection.
//!
//! # Why the copy-chord dance
//!
//! There is no cross-platform API to read *another* application's selection —
//! the highlighted text lives in that app's process and the OS does not hand
//! it out. The one channel every app already honours is copy. So: save the
//! user's clipboard, synthesise the platform copy chord (Ctrl+C / Cmd+C) into
//! the focused app, read back what landed, restore the clipboard. The
//! save/restore is [`SavedClipboard`]'s discipline, not optional — the
//! clipboard is the user's data, and leaving their selection sitting on it
//! would be the same sin `inject` was built to prevent.
//!
//! # Why this runs at key-*down*, not at finalize
//!
//! The selection only exists while the user is still holding it. Reading at
//! key-up — after seconds of recording and any stray click — would copy
//! whatever happens to be highlighted then, usually nothing. And the copy
//! chord has to go to the app that had focus *when the hotkey fired*; later,
//! that guarantee is gone. So the read is spawned at arm time in the same
//! `spawn_blocking`-at-key-down pattern as the screenshot leg (invariant #5):
//! it costs ~50 ms while the user is still speaking and is long finished by
//! the time `finalize` collects it.
//!
//! Everything here blocks (`enigo`, `arboard`, and a settle sleep), so the
//! caller must go through `spawn_blocking` (invariant #6) — the pipeline does.

use std::thread::sleep;

use crate::error::{AppError, Result};
use crate::inject::{self, clipboard::SavedClipboard, keyboard};

/// Reads the text currently highlighted in whichever app has focus.
///
/// Save clipboard → copy chord → settle → read → restore (in `Drop`). Empty,
/// whitespace-only, or unchanged clipboards all mean "the user has nothing
/// selected" and come back as [`AppError::NoSelection`].
pub fn read_selection() -> Result<String> {
    let mut saved = SavedClipboard::capture()?;
    keyboard::copy()?;
    // The chord is a message: the target app answers it when it processes its
    // event queue, not when we send it — the same settle discipline a paste
    // applies before the clipboard is restored (`inject::PASTE_SETTLE`).
    sleep(inject::PASTE_SETTLE);
    let copied = saved.get_text();
    selection_from(saved.previous_text(), copied)
    // `saved` drops here, putting the user's previous clipboard back — the
    // selection copy was borrowed, never ours to keep.
}

/// What a post-copy clipboard read means, as a pure decision so it is
/// testable.
///
/// The unchanged-clipboard guard is load-bearing: with nothing selected, most
/// apps leave the clipboard alone on Ctrl+C, so the read returns the user's
/// own previous clipboard. Treating that as the selection would transform
/// stale clipboard text and type it over the cursor. The false negative —
/// the selected text happens to equal the clipboard — costs one clear
/// `NoSelection` error instead, which is the right side to be wrong on.
fn selection_from(previous: Option<&str>, copied: Option<String>) -> Result<String> {
    let Some(copied) = copied else {
        return Err(AppError::NoSelection);
    };
    if copied.trim().is_empty() {
        return Err(AppError::NoSelection);
    }
    if previous.is_some_and(|previous| previous == copied) {
        return Err(AppError::NoSelection);
    }
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_selection_is_returned_verbatim() {
        let selection = selection_from(Some("old clipboard"), Some("  hey whats up  ".into()));
        assert_eq!(selection.unwrap(), "  hey whats up  ");
    }

    #[test]
    fn nothing_copied_is_no_selection() {
        assert!(matches!(
            selection_from(Some("old clipboard"), None),
            Err(AppError::NoSelection)
        ));
    }

    #[test]
    fn empty_and_whitespace_copies_are_no_selection() {
        for copied in ["", "   ", " \n\t "] {
            assert!(matches!(
                selection_from(None, Some(copied.into())),
                Err(AppError::NoSelection)
            ));
        }
    }

    #[test]
    fn an_unchanged_clipboard_means_nothing_was_selected() {
        assert!(matches!(
            selection_from(Some("the user's own clipboard"), Some("the user's own clipboard".into())),
            Err(AppError::NoSelection)
        ));
    }

    #[test]
    fn whitespace_difference_from_previous_still_counts_as_selected() {
        // The guard compares the *raw* strings: a copy that differs at all
        // really did change the clipboard.
        let selection = selection_from(Some("word"), Some("word ".into()));
        assert_eq!(selection.unwrap(), "word ");
    }
}
