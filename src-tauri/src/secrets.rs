//! API key storage.
//!
//! # Resolution order
//!
//! 1. **Environment variable** (`GROQ_API_KEY`, `OPENAI_API_KEY`,
//!    `ANTHROPIC_API_KEY`). Checked first so a user who manages secrets properly
//!    never has a key written anywhere by this app at all.
//! 2. **The OS credential store** (Windows Credential Manager / macOS Keychain)
//!    via the `keyring` crate, for keys pasted into the settings UI.
//!
//! # Migration from `secrets.json`
//!
//! Keys used to live in a plaintext `secrets.json` in the app config dir. The
//! keychain is a drop-in swap behind this module's four functions, so nothing
//! above this file changed when it landed. The legacy file is handled by
//! **migrate-on-read**: the first `get` that misses in the keychain checks the
//! file, writes any key it finds through to the keychain, and removes the
//! plaintext entry (deleting the file once nothing is left in it). `set` and
//! `clear` also scrub the legacy entry for the credential they touch, so no
//! path can resurrect a plaintext copy.
//!
//! If the keychain write fails mid-migration the plaintext entry is *kept* and
//! the value is still returned — losing the user's only copy of a key is worse
//! than leaving it on disk for one more run, and the migration retries on the
//! next read.
//!
//! # Blocking
//!
//! `keyring` talks to the OS synchronously, so per invariant #6 every public
//! function here is an async wrapper that runs the real work in
//! `spawn_blocking`. The sync bodies are private.
//!
//! Keys are never logged, never emitted in events, and `status` returns only
//! whether a key is present plus a masked hint — never the key itself.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};

use crate::error::{AppError, Result};

/// The credential store account namespace. Changing it orphans every stored
/// key, so it is fixed for the life of the app.
const SERVICE_NAME: &str = "com.whisperscreen.app";

/// Pre-keychain storage, kept only so migrate-on-read can find old keys.
const LEGACY_FILE_NAME: &str = "secrets.json";

/// Which credential a caller wants. Not `SttProvider`/`CleanupProvider` because
/// several of those share one key (Groq speech and Groq chat) and some need none
/// (Ollama, local Whisper).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Credential {
    Groq,
    OpenAi,
    Anthropic,
}

impl Credential {
    pub fn env_var(self) -> &'static str {
        match self {
            Self::Groq => "GROQ_API_KEY",
            Self::OpenAi => "OPENAI_API_KEY",
            Self::Anthropic => "ANTHROPIC_API_KEY",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Groq => "Groq",
            Self::OpenAi => "OpenAI",
            Self::Anthropic => "Anthropic",
        }
    }

    /// The keychain account name. Part of the on-disk credential store format,
    /// so it must never change for an existing credential.
    pub fn store_key(self) -> &'static str {
        match self {
            Self::Groq => "groq",
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
        }
    }

    pub fn all() -> [Credential; 3] {
        [Self::Groq, Self::OpenAi, Self::Anthropic]
    }
}

/// What the UI is allowed to know about a stored key.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialStatus {
    pub credential: Credential,
    pub label: &'static str,
    pub present: bool,
    /// True when the value came from the environment, so the UI can show it as
    /// read-only rather than letting the user "edit" something it cannot change.
    pub from_env: bool,
    /// `gsk_…9c1f` — enough to tell two keys apart, not enough to use one.
    pub hint: Option<String>,
}

/// The legacy plaintext format. Retained only for migrate-on-read.
type LegacyStore = BTreeMap<Credential, String>;

fn legacy_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf> {
    Ok(app.path().app_config_dir()?.join(LEGACY_FILE_NAME))
}

fn read_legacy<R: Runtime>(app: &AppHandle<R>) -> LegacyStore {
    let Ok(path) = legacy_path(app) else {
        return LegacyStore::new();
    };
    if !path.exists() {
        return LegacyStore::new();
    }
    match fs::read_to_string(&path).map(|raw| serde_json::from_str::<LegacyStore>(&raw)) {
        Ok(Ok(store)) => store,
        // Deliberately terse: a parse error here must not put key material or a
        // file fragment into the log.
        _ => {
            log::error!("{LEGACY_FILE_NAME} is unreadable; treating it as empty");
            LegacyStore::new()
        }
    }
}

/// Writes the legacy store back after an entry was migrated out, atomically
/// (temp + rename, same as settings). An empty store means the migration is
/// complete, so the file itself is deleted.
fn write_legacy<R: Runtime>(app: &AppHandle<R>, store: &LegacyStore) -> Result<()> {
    let path = legacy_path(app)?;
    if store.is_empty() {
        match fs::remove_file(&path) {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err.into()),
        }
    }
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, serde_json::to_string_pretty(store)?)?;
    fs::rename(&temp, &path)?;
    Ok(())
}

/// Drops the legacy plaintext entry for one credential, if there is one.
fn scrub_legacy<R: Runtime>(app: &AppHandle<R>, credential: Credential) -> Result<()> {
    let mut store = read_legacy(app);
    if store.remove(&credential).is_some() {
        write_legacy(app, &store)?;
    }
    Ok(())
}

fn entry(credential: Credential) -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE_NAME, credential.store_key())
        .map_err(|err| AppError::Secrets(err.to_string()))
}

/// Trims a candidate value and drops empties, so a whitespace-only env var or
/// stored value behaves as "unset" everywhere.
fn clean(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The keychain's answer for one credential. Missing and unreadable are both
/// `None` — but an unreadable store is worth a warning, because every lookup
/// falling through to migration would silently re-check the legacy file.
fn keychain_get(credential: Credential) -> Result<Option<String>> {
    match entry(credential)?.get_password() {
        Ok(value) => Ok(clean(Some(value))),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => {
            log::warn!("keychain read for {} failed: {err}", credential.label());
            Ok(None)
        }
    }
}

/// Resolves a credential, or errors with a message the user can act on.
pub async fn get<R: Runtime>(app: &AppHandle<R>, credential: Credential) -> Result<String> {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || get_sync(&app, credential))
        .await
        .map_err(|err| AppError::Secrets(format!("keychain task failed: {err}")))?
}

fn get_sync<R: Runtime>(app: &AppHandle<R>, credential: Credential) -> Result<String> {
    if let Some(value) = clean(std::env::var(credential.env_var()).ok()) {
        return Ok(value);
    }

    if let Some(value) = keychain_get(credential)? {
        return Ok(value);
    }

    // Migrate-on-read: a key left over in the legacy plaintext file is written
    // through to the keychain and then removed from disk.
    let mut store = read_legacy(app);
    if let Some(value) = clean(store.get(&credential).cloned()) {
        match entry(credential)?.set_password(&value) {
            Ok(()) => {
                store.remove(&credential);
                if let Err(err) = write_legacy(app, &store) {
                    // The key is safe in the keychain; a leftover plaintext
                    // copy is a housekeeping failure, not a lost key.
                    log::error!("could not remove the migrated plaintext key: {err}");
                }
                log::info!("migrated the {} API key into the OS credential store", credential.label());
            }
            Err(err) => {
                // Keep the plaintext copy (see the module doc) and still hand
                // the key back — the app should keep working on a machine
                // whose credential store is broken.
                log::error!("could not migrate the {} key into the keychain: {err}", credential.label());
            }
        }
        return Ok(value);
    }

    Err(AppError::MissingCredential {
        label: credential.label(),
        env_var: credential.env_var(),
    })
}

/// Stores a key. An empty or whitespace-only value clears it instead, so the UI
/// does not need a separate delete affordance.
pub async fn set<R: Runtime>(app: &AppHandle<R>, credential: Credential, value: &str) -> Result<()> {
    let app = app.clone();
    let value = value.trim().to_string();
    tauri::async_runtime::spawn_blocking(move || set_sync(&app, credential, &value))
        .await
        .map_err(|err| AppError::Secrets(format!("keychain task failed: {err}")))?
}

fn set_sync<R: Runtime>(app: &AppHandle<R>, credential: Credential, value: &str) -> Result<()> {
    let entry = entry(credential)?;
    if value.is_empty() {
        match entry.delete_credential() {
            // Deleting what was never stored is still "cleared".
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(err) => return Err(AppError::Secrets(err.to_string())),
        }
    } else {
        entry
            .set_password(value)
            .map_err(|err| AppError::Secrets(err.to_string()))?;
    }
    // The keychain is the only store now — no path may leave a plaintext copy.
    scrub_legacy(app, credential)
}

pub async fn clear<R: Runtime>(app: &AppHandle<R>, credential: Credential) -> Result<()> {
    set(app, credential, "").await
}

/// Presence and a masked hint for every credential. Never returns key material.
///
/// Read-only by design: it reports legacy keys as present (with a hint) but
/// leaves the migration to `get`, so rendering the settings window has no side
/// effects on disk.
pub async fn status<R: Runtime>(app: &AppHandle<R>) -> Vec<CredentialStatus> {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || status_sync(&app))
        .await
        .unwrap_or_else(|err| {
            log::error!("keychain task failed: {err}");
            Vec::new()
        })
}

fn status_sync<R: Runtime>(app: &AppHandle<R>) -> Vec<CredentialStatus> {
    let store = read_legacy(app);

    Credential::all()
        .into_iter()
        .map(|credential| {
            let env = clean(std::env::var(credential.env_var()).ok());
            let from_env = env.is_some();
            let value = env
                .or_else(|| keychain_get(credential).ok().flatten())
                .or_else(|| clean(store.get(&credential).cloned()));

            CredentialStatus {
                credential,
                label: credential.label(),
                present: value.is_some(),
                from_env,
                hint: value.as_deref().map(mask),
            }
        })
        .collect()
}

/// `gsk_…9c1f`. Shows the prefix (which identifies the provider) and the last
/// four characters (which identify the key), and nothing in between.
fn mask(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 8 {
        // Short enough that any revealed character is a meaningful fraction of
        // the secret. Reveal nothing.
        return "…".repeat(chars.len().min(4));
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_hides_the_middle() {
        assert_eq!(mask("gsk_abcdefghijkl9c1f"), "gsk_…9c1f");
    }

    #[test]
    fn mask_never_echoes_a_short_value() {
        assert!(!mask("abcd1234").contains('a'));
        assert!(!mask("hi").contains('h'));
    }

    #[test]
    fn env_var_names_are_the_conventional_ones() {
        assert_eq!(Credential::Groq.env_var(), "GROQ_API_KEY");
        assert_eq!(Credential::OpenAi.env_var(), "OPENAI_API_KEY");
        assert_eq!(Credential::Anthropic.env_var(), "ANTHROPIC_API_KEY");
    }

    #[test]
    fn credential_is_a_stable_json_key() {
        // `LegacyStore` is a map keyed by this enum, so its serialised form is
        // part of the legacy on-disk format migrate-on-read must keep parsing.
        let json = serde_json::to_string(&Credential::OpenAi).unwrap();
        assert_eq!(json, "\"openAi\"");
    }

    #[test]
    fn store_keys_are_stable_keychain_account_names() {
        // Renaming one of these orphans the stored key for that provider.
        assert_eq!(Credential::Groq.store_key(), "groq");
        assert_eq!(Credential::OpenAi.store_key(), "openai");
        assert_eq!(Credential::Anthropic.store_key(), "anthropic");
    }

    #[test]
    fn clean_trims_and_treats_whitespace_as_unset() {
        assert_eq!(clean(Some("  sk-abc \n".into())), Some("sk-abc".into()));
        assert_eq!(clean(Some("   ".into())), None);
        assert_eq!(clean(None), None);
    }
}
