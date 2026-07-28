//! One shared HTTP client for every provider.
//!
//! Built once and reused. That is not a micro-optimisation: a fresh `Client` per
//! request means a fresh TLS handshake, which is 100–300 ms against a remote API
//! — on its own enough to blow the 800 ms budget. Reusing the client keeps the
//! connection pool warm so the second dictation of a session is materially
//! faster than the first.

use std::sync::OnceLock;
use std::time::Duration;

use crate::error::{AppError, Result};

static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// The process-wide client. Cheap to clone; `reqwest::Client` is an `Arc` inside.
pub fn client() -> Result<reqwest::Client> {
    if let Some(client) = CLIENT.get() {
        return Ok(client.clone());
    }

    let client = reqwest::Client::builder()
        // Keep sockets alive between utterances so repeat dictation skips the
        // handshake entirely.
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(4)
        // Fail fast on connect: if the network is gone, say so rather than
        // hanging until the per-request timeout.
        .connect_timeout(Duration::from_secs(8))
        .user_agent(concat!("whisper-screen/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|err| AppError::Http(format!("could not build http client: {err}")))?;

    // A concurrent caller may have won the race; either instance is fine.
    Ok(CLIENT.get_or_init(|| client).clone())
}

/// A short, actionable description of a transport failure.
///
/// `reqwest`'s `Display` is verbose and buries the useful part. The UI shows this
/// string directly, so it needs to answer "what do I do about it".
pub fn describe(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        return "the provider did not respond in time".to_string();
    }
    if err.is_connect() {
        return "could not reach the provider — check your connection".to_string();
    }
    if err.is_decode() {
        return "the provider sent a response we could not decode".to_string();
    }
    err.to_string()
}

/// Caps a provider error body so a 2 MB HTML error page cannot end up in a log
/// line or a UI toast.
pub fn truncate(body: &str) -> String {
    const LIMIT: usize = 400;
    let body = body.trim();
    if body.chars().count() <= LIMIT {
        return body.to_string();
    }
    let clipped: String = body.chars().take(LIMIT).collect();
    format!("{clipped}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_bodies_pass_through_unchanged() {
        assert_eq!(truncate("  invalid api key  "), "invalid api key");
    }

    #[test]
    fn long_bodies_are_clipped_with_an_ellipsis() {
        let long = "x".repeat(5_000);
        let out = truncate(&long);
        assert_eq!(out.chars().count(), 401);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn truncate_counts_characters_not_bytes() {
        // Slicing by byte index would panic mid-codepoint here.
        let long = "é".repeat(1_000);
        assert_eq!(truncate(&long).chars().count(), 401);
    }
}
