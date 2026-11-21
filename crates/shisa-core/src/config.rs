//! Per-user policy file, `/etc/shisa/users/<username>.toml` (docs/subsystems/configuration.md).

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
use std::path::PathBuf;

/// Upper bound for the authentication wait. A `Type=exec` unit counts the
/// time a held exec takes against its 90 s start timeout (docs/subsystems/configuration.md).
pub const MAX_PROMPT_TIMEOUT: u64 = 85;
pub const MIN_PROMPT_TIMEOUT: u64 = 5;
/// Upper bound for a grace period, one year. A ticket's expiry is the time
/// of the authentication plus the grace period, and that sum must stay
/// representable: an absurd value would overflow and abort shisad.
pub const MAX_GRACE: u64 = 365 * 24 * 3600;
/// Upper bound for the unfocus relock delay. A day is already longer than
/// any window stays open without being touched.
pub const MAX_RELOCK_SECONDS: u64 = 24 * 3600;
/// Defaults for both the file format and the settings window, which shows
/// them to a user who has no config file yet.
pub const DEFAULT_GRACE_SECONDS: u64 = 300;
pub const DEFAULT_PROMPT_TIMEOUT_SECONDS: u64 = 60;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserConfig {
    #[serde(default)]
    pub global: Global,
    #[serde(default)]
    pub apps: Vec<App>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Global {
    pub grace_seconds: u64,
    pub clear_on_screen_lock: bool,
    pub prompt_timeout_seconds: u64,
}

impl Default for Global {
    fn default() -> Self {
        Global {
            grace_seconds: DEFAULT_GRACE_SECONDS,
            clear_on_screen_lock: true,
            prompt_timeout_seconds: DEFAULT_PROMPT_TIMEOUT_SECONDS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct App {
    pub id: String,
    #[serde(default)]
    pub paths: Vec<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grace_seconds: Option<u64>,
    #[serde(default)]
    pub autostart: Autostart,
    #[serde(default)]
    pub relock_after_unfocus_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Autostart {
    #[default]
    Prompt,
    Allow,
}

#[derive(Debug)]
pub struct ConfigError(String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

/// An application id is a desktop file id without the `.desktop` suffix.
/// Spaces are allowed: desktop files may contain them, and an id that
/// `installed` can return must be one `SetProtected` accepts.
pub fn valid_app_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 255
        && !id.starts_with('.')
        && !id.chars().any(|c| c == '/' || c.is_control())
}

impl UserConfig {
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let cfg: UserConfig = toml::from_str(text).map_err(|e| ConfigError(e.to_string()))?;
        let mut seen = HashSet::new();
        for app in &cfg.apps {
            if !valid_app_id(&app.id) {
                return Err(ConfigError(format!("invalid app id {:?}", app.id)));
            }
            if !seen.insert(app.id.as_str()) {
                return Err(ConfigError(format!("app {:?} listed twice", app.id)));
            }
            if let Some(p) = app.paths.iter().find(|p| !p.is_absolute()) {
                return Err(ConfigError(format!(
                    "app {:?}: path {:?} is not absolute",
                    app.id,
                    p.display()
                )));
            }
        }
        Ok(cfg)
    }

    /// Grace period of one app, falling back to the global value.
    pub fn grace_seconds(&self, app: &App) -> u64 {
        app.grace_seconds
            .unwrap_or(self.global.grace_seconds)
            .min(MAX_GRACE)
    }

    /// Grace period for apps without their own, kept inside [`MAX_GRACE`].
    pub fn global_grace_seconds(&self) -> u64 {
        self.global.grace_seconds.min(MAX_GRACE)
    }

    /// Turn the configuration back into the file format. shisad writes this
    /// after a settings change (docs/subsystems/configuration.md).
    pub fn to_toml(&self) -> Result<String, ConfigError> {
        toml::to_string(self).map_err(|e| ConfigError(e.to_string()))
    }

    /// Authentication wait, kept inside the range that is safe for systemd.
    pub fn prompt_timeout_seconds(&self) -> u64 {
        self.global
            .prompt_timeout_seconds
            .clamp(MIN_PROMPT_TIMEOUT, MAX_PROMPT_TIMEOUT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_ids_may_contain_spaces() {
        assert!(valid_app_id("My App"));
        assert!(!valid_app_id("../etc"));
        assert!(!valid_app_id("a/b"));
        assert!(!valid_app_id(""));
    }

    #[test]
    fn design_example_parses() {
        let cfg = UserConfig::parse(
            r#"
            [global]
            grace_seconds = 300
            clear_on_screen_lock = true
            prompt_timeout_seconds = 60

            [[apps]]
            id = "firefox-nightly"
            paths = ["/usr/lib/firefox-nightly/firefox"]
            grace_seconds = 600
            autostart = "prompt"
            relock_after_unfocus_seconds = 0
            "#,
        )
        .unwrap();
        assert_eq!(cfg.apps.len(), 1);
        assert_eq!(cfg.grace_seconds(&cfg.apps[0]), 600);
        assert_eq!(cfg.apps[0].autostart, Autostart::Prompt);
    }

    #[test]
    fn defaults_apply() {
        let cfg = UserConfig::parse("[[apps]]\nid = \"org.kde.kwrite\"\n").unwrap();
        assert_eq!(cfg.global, Global::default());
        assert_eq!(cfg.grace_seconds(&cfg.apps[0]), 300);
        assert!(cfg.apps[0].paths.is_empty());
        assert!(UserConfig::parse("").unwrap().apps.is_empty());
    }

    #[test]
    fn rejects_bad_input() {
        assert!(UserConfig::parse("[[apps]]\nid = \"a/b\"\n").is_err());
        assert!(UserConfig::parse("[[apps]]\nid = \"\"\n").is_err());
        assert!(UserConfig::parse("[[apps]]\nid = \"a\"\npaths = [\"bin/a\"]\n").is_err());
        assert!(UserConfig::parse("[[apps]]\nid = \"a\"\n[[apps]]\nid = \"a\"\n").is_err());
        assert!(UserConfig::parse("[[apps]]\nid = \"a\"\ngrace = 3\n").is_err());
        assert!(UserConfig::parse("[global]\ngrace_seconds = -1\n").is_err());
        assert!(UserConfig::parse("[[apps]]\nid = \"a\"\nautostart = \"yes\"\n").is_err());
    }

    #[test]
    fn serializes_and_parses_back() {
        let text = r#"
            [global]
            grace_seconds = 120

            [[apps]]
            id = "app"
            paths = ["/usr/bin/app"]
            autostart = "allow"
        "#;
        let cfg = UserConfig::parse(text).unwrap();
        let written = cfg.to_toml().unwrap();
        assert_eq!(UserConfig::parse(&written).unwrap(), cfg);
        assert!(written.contains("grace_seconds = 120"));
        assert!(written.contains("paths = [\"/usr/bin/app\"]"));
        assert!(written.contains("autostart = \"allow\""));
        assert!(!written.contains("None"));
    }

    #[test]
    fn grace_is_capped() {
        let cfg = UserConfig::parse(
            "[global]\ngrace_seconds = 9223372036854775807\n[[apps]]\nid = \"a\"\n[[apps]]\nid = \"b\"\ngrace_seconds = 9223372036854775807\n",
        )
        .unwrap();
        assert_eq!(cfg.global_grace_seconds(), MAX_GRACE);
        assert_eq!(cfg.grace_seconds(&cfg.apps[0]), MAX_GRACE);
        assert_eq!(cfg.grace_seconds(&cfg.apps[1]), MAX_GRACE);
    }

    #[test]
    fn prompt_timeout_is_clamped() {
        let high = UserConfig::parse("[global]\nprompt_timeout_seconds = 600\n").unwrap();
        assert_eq!(high.prompt_timeout_seconds(), MAX_PROMPT_TIMEOUT);
        let low = UserConfig::parse("[global]\nprompt_timeout_seconds = 0\n").unwrap();
        assert_eq!(low.prompt_timeout_seconds(), MIN_PROMPT_TIMEOUT);
    }
}
