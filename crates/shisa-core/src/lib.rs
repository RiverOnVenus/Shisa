//! Types shared by shisad and the user-side tools.

pub mod config;
pub mod dbus;
pub mod desktop;
pub mod users;

/// Per-user policy files live here, one `<username>.toml` each (DESIGN 7.1).
pub const CONFIG_DIR: &str = "/etc/shisa/users";
