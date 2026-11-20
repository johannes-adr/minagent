//! The web UI's login: one user, one password. Only an Argon2id hash of the password is
//! stored, in `auth.json` next to the settings file (mode 0600, git-ignored). While there is
//! no password the page offers to create one.
//!
//! A successful login yields a random session token, kept in memory (a restart logs everybody
//! out), which the page sends as `x-token` like the token printed at startup.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rig_core::serde_json::{self, json};
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

pub const FILE: &str = "auth.json";
/// A wrong password is answered only after this long, and attempts are handled one at a time.
const FAILURE_DELAY: Duration = Duration::from_secs(1);
/// So many attempts may be in progress or waiting; further ones are refused at once.
const MAX_ATTEMPTS: usize = 5;

/// The auth file next to the settings file.
pub fn path_for(config: &Path) -> PathBuf {
    config.with_file_name(FILE)
}

pub enum SetupError {
    /// There is a password already.
    Exists,
    /// An empty password would be no password at all.
    Empty,
    Failed(String),
    /// Too many attempts at once, see [`MAX_ATTEMPTS`].
    Busy,
}

#[derive(Debug)]
pub enum LoginError {
    Wrong,
    /// Too many attempts at once, see [`MAX_ATTEMPTS`].
    Busy,
}

/// Counts an attempt while it is in progress or waiting.
struct Attempt<'a>(&'a AtomicUsize);

impl<'a> Attempt<'a> {
    fn start(count: &'a AtomicUsize) -> Option<Self> {
        if count.fetch_add(1, Ordering::SeqCst) >= MAX_ATTEMPTS {
            count.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Self(count))
    }
}

impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct Auth {
    path: PathBuf,
    /// The PHC string of the password hash; `None` until one is set.
    hash: Mutex<Option<String>>,
    sessions: Mutex<HashSet<String>>,
    /// Held while a password is checked, so that guesses can't be made in parallel.
    attempts: tokio::sync::Mutex<()>,
    waiting: AtomicUsize,
}

impl Auth {
    pub fn load(path: PathBuf) -> io::Result<Self> {
        let hash = match std::fs::read_to_string(&path) {
            Ok(text) => {
                let value: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|e| io::Error::other(format!("{}: {e}", path.display())))?;
                value["password_hash"].as_str().map(str::to_owned)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        Ok(Self {
            path,
            hash: Mutex::new(hash),
            sessions: Mutex::default(),
            attempts: tokio::sync::Mutex::new(()),
            waiting: AtomicUsize::new(0),
        })
    }

    pub fn is_set(&self) -> bool {
        self.hash.lock().unwrap().is_some()
    }

    /// Stores the first password and logs in. Returns the session token.
    pub async fn setup(&self, password: &str) -> Result<String, SetupError> {
        if password.is_empty() {
            return Err(SetupError::Empty);
        }
        let _count = Attempt::start(&self.waiting).ok_or(SetupError::Busy)?;
        let _attempt = self.attempts.lock().await;
        if self.is_set() {
            return Err(SetupError::Exists);
        }
        let password = password.to_owned();
        let hash = tokio::task::spawn_blocking(move || hash_password(&password))
            .await
            .map_err(|e| SetupError::Failed(e.to_string()))?
            .map_err(SetupError::Failed)?;
        self.write(&hash).map_err(|e| SetupError::Failed(e.to_string()))?;
        *self.hash.lock().unwrap() = Some(hash);
        self.new_session().map_err(|e| SetupError::Failed(e.to_string()))
    }

    /// Checks the password; returns a session token if it is right.
    pub async fn login(&self, password: &str) -> Result<String, LoginError> {
        let _count = Attempt::start(&self.waiting).ok_or(LoginError::Busy)?;
        let _attempt = self.attempts.lock().await;
        let hash = self.hash.lock().unwrap().clone().ok_or(LoginError::Wrong)?;
        let password = password.to_owned();
        let right = tokio::task::spawn_blocking(move || verify(&password, &hash))
            .await
            .unwrap_or(false);
        if !right {
            tokio::time::sleep(FAILURE_DELAY).await;
            return Err(LoginError::Wrong);
        }
        self.new_session().map_err(|_| LoginError::Wrong)
    }

    pub fn valid(&self, token: &str) -> bool {
        self.sessions.lock().unwrap().contains(token)
    }

    pub fn logout(&self, token: &str) {
        self.sessions.lock().unwrap().remove(token);
    }

    fn new_session(&self) -> io::Result<String> {
        let token = super::web::random_token()?;
        self.sessions.lock().unwrap().insert(token.clone());
        Ok(token)
    }

    fn write(&self, hash: &str) -> io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&self.path)?;
        let text = serde_json::to_string_pretty(&json!({"password_hash": hash}))?;
        writeln!(file, "{text}")
    }
}

fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::from_b64(&super::web::random_token().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| e.to_string())
}

fn verify(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .is_ok_and(|parsed| Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agent_auth_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(FILE)
    }

    #[tokio::test]
    async fn the_first_password_is_set_up_once_and_survives_a_restart() {
        let path = temp_path("setup");
        let auth = Auth::load(path.clone()).unwrap();
        assert!(!auth.is_set());
        assert!(matches!(auth.setup("").await, Err(SetupError::Empty)));
        let token = auth.setup("pw").await.ok().unwrap();
        assert!(auth.valid(&token));
        assert!(matches!(auth.setup("another").await, Err(SetupError::Exists)));

        // Only a hash is stored.
        let stored = std::fs::read_to_string(&path).unwrap();
        assert!(stored.contains("$argon2id$") && !stored.contains("pw\""));

        let again = Auth::load(path).unwrap();
        assert!(again.is_set());
        assert!(matches!(again.login("wrong password").await, Err(LoginError::Wrong)));
        let token = again.login("pw").await.unwrap();
        assert!(again.valid(&token));
        again.logout(&token);
        assert!(!again.valid(&token));
    }

    #[tokio::test]
    async fn a_flood_of_guesses_is_refused_instead_of_queued() {
        let auth = std::sync::Arc::new(Auth::load(temp_path("flood")).unwrap());
        auth.setup("pw").await.ok().unwrap();
        let guesses: Vec<_> = (0..MAX_ATTEMPTS + 4)
            .map(|_| {
                let auth = auth.clone();
                tokio::spawn(async move { auth.login("wrong").await })
            })
            .collect();
        let mut busy = 0;
        for guess in guesses {
            if matches!(guess.await.unwrap(), Err(LoginError::Busy)) {
                busy += 1;
            }
        }
        assert_eq!(busy, 4);
        // Once they are done, the right password works again.
        assert!(auth.login("pw").await.is_ok());
    }
}
