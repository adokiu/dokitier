use std::collections::HashMap;
use std::time::{Duration, Instant};

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand::RngCore;
use tokio::sync::RwLock;

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|err| anyhow::anyhow!("failed to hash password: {err}"))
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

#[derive(Default)]
pub struct Sessions {
    ttl: Duration,
    inner: RwLock<HashMap<String, Instant>>,
}

impl Sessions {
    pub fn new(ttl_hours: u64) -> Self {
        Self {
            ttl: Duration::from_secs(ttl_hours.max(1) * 3600),
            inner: RwLock::new(HashMap::new()),
        }
    }

    pub async fn create(&self) -> String {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        let token = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, bytes);
        let mut map = self.inner.write().await;
        map.retain(|_, expires| *expires > Instant::now());
        map.insert(token.clone(), Instant::now() + self.ttl);
        token
    }

    pub async fn validate(&self, token: &str) -> bool {
        let mut map = self.inner.write().await;
        match map.get(token) {
            Some(expires) if *expires > Instant::now() => true,
            Some(_) => {
                map.remove(token);
                false
            }
            None => false,
        }
    }

    pub async fn remove(&self, token: &str) {
        self.inner.write().await.remove(token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_roundtrip() {
        let hash = hash_password("s3cret").unwrap();
        assert!(verify_password("s3cret", &hash));
        assert!(!verify_password("wrong", &hash));
    }

    #[tokio::test]
    async fn sessions_expire() {
        let sessions = Sessions::new(1);
        let token = sessions.create().await;
        assert!(sessions.validate(&token).await);
        assert!(!sessions.validate("nope").await);
        sessions.remove(&token).await;
        assert!(!sessions.validate(&token).await);
    }
}
