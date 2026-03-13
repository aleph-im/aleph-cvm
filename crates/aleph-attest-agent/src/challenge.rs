use std::sync::Mutex;
use std::time::{Duration, Instant};

use actix_web::{HttpResponse, web};
use rand::Rng;

/// How long a challenge nonce remains valid.
const CHALLENGE_TTL: Duration = Duration::from_secs(300); // 5 minutes

/// A time-limited challenge nonce.
struct Challenge {
    nonce: [u8; 32],
    issued_at: Instant,
}

/// Thread-safe store for at most one active challenge nonce.
pub struct ChallengeStore {
    inner: Mutex<Option<Challenge>>,
}

impl ChallengeStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    /// Generate a fresh 32-byte random nonce, replacing any existing challenge.
    pub fn issue(&self) -> [u8; 32] {
        let nonce: [u8; 32] = rand::rng().random();
        let challenge = Challenge {
            nonce,
            issued_at: Instant::now(),
        };
        let mut guard = self.inner.lock().expect("challenge lock poisoned");
        *guard = Some(challenge);
        nonce
    }

    /// Consume the stored nonce if it matches `nonce` and has not expired.
    /// Returns `Some(nonce)` on success, `None` if no challenge exists,
    /// the nonce doesn't match, or the TTL has elapsed.
    #[cfg(test)]
    pub fn consume(&self, nonce: &[u8; 32]) -> Option<[u8; 32]> {
        let mut guard = self.inner.lock().expect("challenge lock poisoned");
        let challenge = guard.as_ref()?;
        if challenge.nonce != *nonce {
            return None;
        }
        if challenge.issued_at.elapsed() > CHALLENGE_TTL {
            *guard = None;
            return None;
        }
        let stored = challenge.nonce;
        *guard = None;
        Some(stored)
    }

    /// Consume the stored nonce regardless of value, if it has not expired.
    /// Returns `Some(nonce)` on success, `None` if no challenge exists or has expired.
    pub fn consume_any(&self) -> Option<[u8; 32]> {
        let mut guard = self.inner.lock().expect("challenge lock poisoned");
        let challenge = guard.as_ref()?;
        if challenge.issued_at.elapsed() > CHALLENGE_TTL {
            *guard = None;
            return None;
        }
        let stored = challenge.nonce;
        *guard = None;
        Some(stored)
    }
}

/// GET /confidential/challenge
///
/// Issues a fresh 32-byte challenge nonce and returns it as hex.
pub async fn challenge_endpoint(store: web::Data<ChallengeStore>) -> HttpResponse {
    let nonce = store.issue();
    HttpResponse::Ok().json(serde_json::json!({ "nonce": hex::encode(nonce) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_and_consume() {
        let store = ChallengeStore::new();
        let nonce = store.issue();
        assert_eq!(store.consume(&nonce), Some(nonce));
    }

    #[test]
    fn consumed_nonce_is_rejected() {
        let store = ChallengeStore::new();
        let nonce = store.issue();
        assert!(store.consume(&nonce).is_some());
        // Second consume must fail.
        assert!(store.consume(&nonce).is_none());
    }

    #[test]
    fn wrong_nonce_is_rejected() {
        let store = ChallengeStore::new();
        let _nonce = store.issue();
        let wrong = [0xFFu8; 32];
        assert!(store.consume(&wrong).is_none());
    }

    #[test]
    fn new_issue_replaces_old() {
        let store = ChallengeStore::new();
        let first = store.issue();
        let second = store.issue();
        assert_ne!(first, second);
        // Old nonce must not work.
        assert!(store.consume(&first).is_none());
        // New nonce works.
        assert!(store.consume(&second).is_some());
    }

    #[test]
    fn no_challenge_returns_none() {
        let store = ChallengeStore::new();
        let nonce = [0u8; 32];
        assert!(store.consume(&nonce).is_none());
    }
}
