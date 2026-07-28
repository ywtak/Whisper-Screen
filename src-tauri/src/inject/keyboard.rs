//! Synthetic typing and key chords via `enigo`.
//!
//! Everything here **blocks the calling thread** — `enigo` talks to the OS
//! input subsystem synchronously (`SendInput` on Windows, `CGEvent` on macOS).
//! Callers must therefore go through `spawn_blocking`; the async wrapper in
//! [`super`] does exactly that (invariant #6).
//!
//! # Why `enigo.text()` and not per-key synthesis
//!
//! Typing a string key-by-key means mapping every character to a physical
//! key combination under the user's *current* layout — which breaks the
//! moment the text contains Unicode or the layout is not US QWERTY.
//! `Keyboard::text` hands the whole string to the OS text-input path, which
//! handles both. Per-key synthesis is kept only for the copy/paste chords,
//! where a key *event* (not text) is precisely what we want.
//!
//! # macOS Accessibility permission
//!
//! Synthetic key events require the Accessibility grant, which macOS **cannot
//! be requested silently** — the user must enable the app in System Settings →
//! Privacy & Security → Accessibility. Neither `enigo` nor anything else we
//! link exposes a verifiable "is trusted" check (that is
//! `AXIsProcessTrusted` in ApplicationServices, which is not among our
//! dependencies), so we do not guess FFI. Instead [`with_hint`] appends
//! the fix to every injection error on macOS: the user finds out *why*
//! typing failed and *where* to grant it, instead of the text silently never
//! appearing. If a trusted-check dependency is ever added, this function is
//! its one obvious home.

use enigo::{Direction, Enigo, Key, Keyboard, Settings as EnigoSettings};

use crate::error::{AppError, Result};

/// Types `text` into whatever application currently has keyboard focus.
///
/// Uses `Keyboard::text`, not per-key synthesis — see the module doc for why
/// that is the only correct choice for arbitrary (Unicode, non-US-layout)
/// text.
pub fn type_text(text: &str) -> Result<()> {
    let mut enigo = connect()?;
    enigo
        .text(text)
        .map_err(|err| AppError::Inject(with_hint(format!("typing failed: {err}"))))
}

/// Sends the platform paste chord — Ctrl+V on Windows, Cmd+V on macOS.
///
/// This is a deliberate key *event* (the one place per-key synthesis is
/// right): the target application must see a paste shortcut, not the letter
/// "v" arriving while a modifier happens to be held.
pub fn paste() -> Result<()> {
    let mut enigo = connect()?;
    let modifier = chord_modifier();
    let fail = |err: enigo::InputError| {
        AppError::Inject(with_hint(format!("paste shortcut failed: {err}")))
    };
    enigo.key(modifier, Direction::Press).map_err(fail)?;
    let clicked = enigo.key(Key::Unicode('v'), Direction::Click);
    // Always release the modifier, even if the click failed — a stuck Ctrl/Cmd
    // turns the user's next keystrokes into shortcuts.
    let released = enigo.key(modifier, Direction::Release);
    clicked.map_err(fail)?;
    released.map_err(fail)?;
    Ok(())
}

/// Sends the platform copy chord — Ctrl+C on Windows, Cmd+C on macOS.
///
/// Same discipline as [`paste`]: a key *event* the focused app must see as a
/// shortcut, and the modifier is released even when the click fails. This is
/// how `capture::selection` asks another app for its highlighted text — there
/// is no cross-platform API for that, but every app answers copy.
pub fn copy() -> Result<()> {
    let mut enigo = connect()?;
    let modifier = chord_modifier();
    let fail = |err: enigo::InputError| {
        AppError::Inject(with_hint(format!("copy shortcut failed: {err}")))
    };
    enigo.key(modifier, Direction::Press).map_err(fail)?;
    let clicked = enigo.key(Key::Unicode('c'), Direction::Click);
    let released = enigo.key(modifier, Direction::Release);
    clicked.map_err(fail)?;
    released.map_err(fail)?;
    Ok(())
}

/// The modifier half of the copy/paste chord on this platform.
fn chord_modifier() -> Key {
    #[cfg(target_os = "macos")]
    {
        Key::Meta
    }
    #[cfg(not(target_os = "macos"))]
    {
        Key::Control
    }
}

/// Opens the OS input connection. Cheap enough to open per injection; there is
/// no session state worth keeping across calls.
fn connect() -> Result<Enigo> {
    Enigo::new(&EnigoSettings::default())
        .map_err(|err| AppError::Inject(with_hint(format!("could not open input device: {err}"))))
}

/// Why an injection error is reported, with the platform fix attached.
///
/// On macOS the overwhelmingly common cause of an injection failure is the
/// missing Accessibility grant, so every error tells the user where to fix
/// it. On other platforms the error stands on its own.
fn with_hint(message: String) -> String {
    #[cfg(target_os = "macos")]
    {
        format!(
            "{message}. Grant Whisper Screen Accessibility access in \
             System Settings → Privacy & Security → Accessibility, then try again"
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        message
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chord_modifier_matches_the_platform_convention() {
        #[cfg(target_os = "macos")]
        assert_eq!(chord_modifier(), Key::Meta);
        #[cfg(not(target_os = "macos"))]
        assert_eq!(chord_modifier(), Key::Control);
    }

    #[test]
    fn hint_points_at_accessibility_only_on_macos() {
        let hinted = with_hint("boom".into());
        #[cfg(target_os = "macos")]
        assert!(hinted.contains("Accessibility"));
        #[cfg(not(target_os = "macos"))]
        assert_eq!(hinted, "boom");
    }
}
