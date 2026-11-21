//! Numbers about the running daemon, for the settings window's diagnostics
//! card.
//!
//! The reload thread owns everything interesting here and writes it as it
//! works. The bus only reads. Nothing in this module touches the fanotify
//! loop.

use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// The account that holds the app-lock password. It is created by
/// `data/shisa-lock.sysusers` and named by
/// `data/49-shisa-admin.rules`, so it has to match both.
const ADMIN_USER: &str = "shisa-lock";

/// Where a system without a readable shadow file stands. Reporting "no
/// password" would send a working installation to the terminal.
const UNKNOWN_READY: bool = true;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Debug)]
pub struct Diag {
    version: &'static str,
    config_dir: PathBuf,
    started: i64,
    reloaded: AtomicI64,
    failed: AtomicI64,
}

impl Diag {
    pub fn new(config_dir: PathBuf) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            config_dir,
            started: now_ms(),
            reloaded: AtomicI64::new(0),
            failed: AtomicI64::new(0),
        }
    }

    /// A load finished, whatever it found.
    pub fn note_reload(&self) {
        self.reloaded.store(now_ms(), Ordering::Relaxed);
    }

    /// The latest load could not mark every configured path.
    pub fn note_failure(&self) {
        self.failed.store(now_ms(), Ordering::Relaxed);
    }

    /// The latest load marked every configured path. Clearing the time makes
    /// the diagnostics card describe the current state instead of staying red
    /// after a transient failure was fixed.
    pub fn clear_failure(&self) {
        self.failed.store(0, Ordering::Relaxed);
    }

    pub fn version(&self) -> &'static str {
        self.version
    }

    pub fn started(&self) -> i64 {
        self.started
    }

    pub fn reloaded(&self) -> i64 {
        self.reloaded.load(Ordering::Relaxed)
    }

    pub fn failed(&self) -> i64 {
        self.failed.load(Ordering::Relaxed)
    }

    /// One user's policy file, whether or not it exists yet.
    pub fn config_path(&self, name: &str) -> String {
        self.config_dir.join(format!("{name}.toml")).display().to_string()
    }
}

/// Whether the app-lock account has a password. `systemd-sysusers` creates it
/// locked, and until root runs `passwd` every polkit check for the app-lock
/// password fails, so the settings window warns about it from the start.
///
/// Only the one field is read. The hash never leaves this function.
pub fn admin_ready() -> bool {
    match std::fs::read_to_string("/etc/shadow") {
        Ok(text) => password_set(&text, ADMIN_USER),
        Err(_) => UNKNOWN_READY,
    }
}

/// Mirrors `passwd -S`: `!`, `!!` and `*` all mean locked or unset.
fn password_set(text: &str, user: &str) -> bool {
    for line in text.lines() {
        let mut fields = line.split(':');
        if fields.next() != Some(user) {
            continue;
        }
        let Some(hash) = fields.next() else {
            return false;
        };
        return !(hash.is_empty() || hash.starts_with('!') || hash == "*");
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hash_means_the_password_is_set() {
        let shadow = format!(
            "root:$y$j9T$abc:19000::::::\n{ADMIN_USER}:$y$j9T$def:19000::::::\n"
        );
        assert!(password_set(&shadow, ADMIN_USER));
    }

    #[test]
    fn locked_and_unset_accounts_are_not_ready() {
        for field in ["!", "!!", "*", ""] {
            let shadow = format!("{ADMIN_USER}:{field}:19000::::::\n");
            assert!(!password_set(&shadow, ADMIN_USER), "{field:?}");
        }
        assert!(!password_set(ADMIN_USER, ADMIN_USER));
    }

    #[test]
    fn the_config_path_is_the_users_own_file() {
        let diag = Diag::new(PathBuf::from("/etc/shisa/users"));
        assert_eq!(diag.config_path("zhui"), "/etc/shisa/users/zhui.toml");
        assert_eq!(diag.started(), diag.started());
        assert!(diag.started() > 0);
        assert_eq!(diag.reloaded(), 0);
    }

    #[test]
    fn a_complete_load_clears_the_failure_time() {
        let diag = Diag::new(PathBuf::from("/etc/shisa/users"));
        diag.note_failure();
        assert!(diag.failed() > 0);
        diag.clear_failure();
        assert_eq!(diag.failed(), 0);
    }
}
