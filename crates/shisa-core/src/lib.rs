//! Types shared by shisad and the user-side tools.

pub mod config;
pub mod dbus;
pub mod desktop;
pub mod i18n;
pub mod users;

/// Per-user policy files live here, one `<username>.toml` each (docs/subsystems/configuration.md).
pub const CONFIG_DIR: &str = "/etc/shisa/users";
