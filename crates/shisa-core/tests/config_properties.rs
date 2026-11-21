//! Properties of the config file format. The table tests in `config.rs` pin
//! individual cases. These check the rules that must hold for every input:
//! anything the parser accepts writes back unchanged, validators clamp into
//! the documented bounds, and the paths the daemon must refuse stay refused.

use proptest::prelude::*;
use shisa_core::config::{
    App, Autostart, Global, MAX_GRACE, MAX_PROMPT_TIMEOUT, MIN_PROMPT_TIMEOUT, UserConfig,
    valid_app_id,
};
use std::collections::HashSet;
use std::path::PathBuf;

/// An app id the parser accepts: non-empty, no slash, no control character,
/// and not starting with a dot.
fn valid_id() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9][a-zA-Z0-9 ._@-]{0,30}"
}

/// Absolute paths only. The parser checks absoluteness, not existence, so
/// the property tests do not need files on disk.
fn valid_path() -> impl Strategy<Value = PathBuf> {
    "/[a-zA-Z0-9._/-]{1,40}".prop_map(PathBuf::from)
}

fn global() -> impl Strategy<Value = Global> {
    (any::<u64>(), any::<bool>(), any::<u64>()).prop_map(
        |(grace_seconds, clear_on_screen_lock, prompt_timeout_seconds)| Global {
            grace_seconds,
            clear_on_screen_lock,
            prompt_timeout_seconds,
        },
    )
}

fn app() -> impl Strategy<Value = App> {
    (
        valid_id(),
        prop::collection::vec(valid_path(), 0..3),
        prop::option::of(any::<u64>()),
        any::<bool>(),
        any::<u64>(),
    )
        .prop_map(|(id, paths, grace_seconds, allow, relock)| App {
            id,
            paths,
            grace_seconds,
            autostart: if allow {
                Autostart::Allow
            } else {
                Autostart::Prompt
            },
            relock_after_unfocus_seconds: relock,
        })
}

fn config() -> impl Strategy<Value = UserConfig> {
    (global(), prop::collection::vec(app(), 0..4))
        .prop_map(|(global, apps)| UserConfig { global, apps })
        // The parser rejects two apps with one id, so only unique ids reach
        // the round-trip property.
        .prop_filter("unique app ids", |config| {
            let mut seen = HashSet::new();
            config.apps.iter().all(|app| seen.insert(app.id.as_str()))
        })
}

proptest! {
    #[test]
    fn whatever_parses_writes_back_unchanged(config in config()) {
        let text = config.to_toml().unwrap();
        prop_assert_eq!(UserConfig::parse(&text).unwrap(), config);
    }

    #[test]
    fn accessors_stay_inside_the_documented_bounds(config in config()) {
        prop_assert!(config.global_grace_seconds() <= MAX_GRACE);
        let wait = config.prompt_timeout_seconds();
        prop_assert!((MIN_PROMPT_TIMEOUT..=MAX_PROMPT_TIMEOUT).contains(&wait));
        for app in &config.apps {
            prop_assert!(config.grace_seconds(app) <= MAX_GRACE);
        }
    }

    #[test]
    fn a_relative_protected_path_is_refused(id in valid_id(), path in "[a-zA-Z0-9._-][a-zA-Z0-9._/-]{0,39}") {
        let config = UserConfig {
            global: Global::default(),
            apps: vec![App {
                id,
                paths: vec![PathBuf::from(path)],
                grace_seconds: None,
                autostart: Autostart::Prompt,
                relock_after_unfocus_seconds: 0,
            }],
        };
        prop_assert!(UserConfig::parse(&config.to_toml().unwrap()).is_err());
    }

    #[test]
    fn two_apps_with_one_id_are_refused(id in valid_id()) {
        let app = App {
            id: id.clone(),
            paths: vec![PathBuf::from("/usr/bin/app")],
            grace_seconds: None,
            autostart: Autostart::Prompt,
            relock_after_unfocus_seconds: 0,
        };
        let config = UserConfig {
            global: Global::default(),
            apps: vec![app.clone(), app],
        };
        prop_assert!(UserConfig::parse(&config.to_toml().unwrap()).is_err());
    }

    #[test]
    fn an_id_with_a_slash_or_leading_dot_is_refused(id in valid_id()) {
        let slash = format!("{id}/child");
        let dot = format!(".{id}");
        prop_assert!(!valid_app_id(&slash));
        prop_assert!(!valid_app_id(&dot));
    }
}

#[test]
fn the_id_length_limit_is_exact() {
    assert!(valid_app_id(&"a".repeat(255)));
    assert!(!valid_app_id(&"a".repeat(256)));
}
