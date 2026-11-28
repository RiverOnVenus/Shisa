//! Names and interfaces shared by shisad and the user-side tools.
//!
//! shisad owns the name on the system bus. The agent calls `RegisterAgent` so
//! state changes reach it, and the settings window calls `RegisterSettings` so
//! a lock can tell it to exit. `ListApps` and `Lock` act on the calling user
//! only, identified from the bus credentials.

pub const BUS_NAME: &str = "io.github.RiverOnVenus.Shisa";
pub const OBJECT_PATH: &str = "/io/github/RiverOnVenus/Shisa/Manager";
pub const INTERFACE: &str = "io.github.RiverOnVenus.Shisa.Manager";
/// The session agent's window gate. The KWin script calls it on the session
/// bus, and the agent forwards protected window events to shisad.
pub const WINDOW_BUS_NAME: &str = "io.github.RiverOnVenus.Shisa.Window";
pub const WINDOW_PATH: &str = "/io/github/RiverOnVenus/Shisa/Window";
pub const WINDOW_INTERFACE: &str = "io.github.RiverOnVenus.Shisa.Window";
/// polkit actions, defined in io.github.<user>.Shisa.policy.
pub const POLKIT_LAUNCH: &str = "io.github.RiverOnVenus.Shisa.launch";
pub const POLKIT_CHANGE: &str = "io.github.RiverOnVenus.Shisa.change";

/// A service that is absent or too old to answer can use a compatibility
/// fallback. Timeouts and other failures must keep their normal handling.
pub fn method_unavailable(error: &zbus::Error) -> bool {
    matches!(
        error,
        zbus::Error::MethodError(name, _, _) if matches!(
            name.as_str(),
            "org.freedesktop.DBus.Error.UnknownMethod"
                | "org.freedesktop.DBus.Error.UnknownInterface"
                | "org.freedesktop.DBus.Error.ServiceUnknown"
                | "org.freedesktop.DBus.Error.NameHasNoOwner"
        )
    )
}

/// One row of the application list: id, ticket state, configured paths and
/// the paths that were actually marked.
pub type AppRow = (String, bool, Vec<String>, Vec<String>);

/// One launch decision for the activity page: id, unix milliseconds, app id,
/// pid, kind, for a request that waits for the user, when its box gives up
/// (unix milliseconds, 0 when not waiting), and when it was answered
/// (unix milliseconds, 0 while it waits).
pub type LaunchRow = (u64, i64, String, i32, String, i64, i64);

/// The daemon's own numbers for the settings window's diagnostics card:
/// version, start time, last reload, and the last load that could not mark
/// every configured path (unix milliseconds, 0 when the latest load was
/// complete), the caller's protected app count and marked path count, the
/// caller's config file, whether the app-lock account has a password yet, and
/// the last configuration read or parse failure (unix milliseconds, 0 when
/// the latest load read every file).
pub type DiagRow = (String, i64, i64, i64, u64, u64, String, bool, i64);

/// A lock's window report from the session agent: status, script
/// generation, matched windows, concealed windows, and the apps with a
/// window that stayed visible.
pub type LockReport = (u8, u64, u64, u64, Vec<(String, u32)>);

/// Every matched protected window is concealed.
pub const LOCK_CONFIRMED: u8 = 0;
/// Some matched protected windows stayed visible. `LockReport` names them.
pub const LOCK_PARTIAL: u8 = 1;
/// KWin did not report for the requested generation.
pub const LOCK_UNCONFIRMED: u8 = 2;
/// KWin's scripting interface is not available on this desktop.
pub const LOCK_NO_LAYER: u8 = 3;

/// Interpret a report from any agent version. Unknown or inconsistent
/// confirmations must be shown as unconfirmed.
pub fn lock_status(report: &LockReport) -> u8 {
    let (status, _, matched, hidden, missed) = report;
    match *status {
        LOCK_CONFIRMED if matched == hidden && missed.is_empty() => LOCK_CONFIRMED,
        LOCK_PARTIAL if !missed.is_empty() => LOCK_PARTIAL,
        LOCK_NO_LAYER => LOCK_NO_LAYER,
        _ => LOCK_UNCONFIRMED,
    }
}

#[zbus::proxy(
    interface = "io.github.RiverOnVenus.Shisa.Manager",
    default_service = "io.github.RiverOnVenus.Shisa",
    default_path = "/io/github/RiverOnVenus/Shisa/Manager"
)]
pub trait Manager {
    /// App id, whether a ticket is held now, configured paths, and the paths
    /// that were actually marked.
    fn list_apps(&self) -> zbus::Result<Vec<AppRow>>;
    /// Drop tickets and window permissions. An empty app id drops all of them.
    fn lock(&self, app_id: &str) -> zbus::Result<()>;
    /// Hide one app's windows but keep its ticket, so restoring them can
    /// reuse the current grace period. An empty app id is refused.
    fn hide(&self, app_id: &str) -> zbus::Result<()>;
    /// Show a hidden app's windows again without authentication when its
    /// ticket is still valid. False means the caller has to authenticate.
    fn unhide(&self, app_id: &str) -> zbus::Result<bool>;
    /// Authenticate for an app and leave a ticket, as `shisa unlock` does.
    fn unlock(&self, app_id: &str) -> zbus::Result<()>;
    /// Announce this process as the user's agent, for state changes.
    fn register_agent(&self) -> zbus::Result<()>;
    /// Announce this connection as a settings window. A global lock or a
    /// screen lock asks registered windows to exit, which ends polkit's
    /// process-keyed retained authorization (docs/subsystems/authentication.md).
    fn register_settings(&self) -> zbus::Result<()>;
    /// Installed applications: id, name, icon.
    fn list_installed(&self) -> zbus::Result<Vec<(String, String, String)>>;
    /// The caller's recent launches, oldest first, and the totals since the
    /// daemon started: requests, allowed, denied.
    fn recent_launches(&self) -> zbus::Result<(Vec<LaunchRow>, u64, u64, u64)>;
    /// The daemon's numbers for the settings window's diagnostics card.
    fn diagnostics(&self) -> zbus::Result<DiagRow>;
    /// Protected paths of an installed application, and the caveats.
    fn resolve_app(&self, app_id: &str) -> zbus::Result<(Vec<String>, Vec<String>)>;
    /// Effective grace period, autostart flag, protection state and the
    /// unfocus relock delay.
    fn app_config(&self, app_id: &str) -> zbus::Result<(u64, bool, bool, u64)>;
    /// Global grace period, whether the screen lock drops tickets, and the
    /// wait limit.
    fn global_config(&self) -> zbus::Result<(u64, bool, u64)>;
    /// Generation that invalidates a settings window's retained polkit
    /// authorization after a global or screen lock.
    fn settings_generation(&self) -> zbus::Result<u64>;
    /// Protect or unprotect an app. Needs authentication first.
    fn set_protected(&self, app_id: &str, on: bool) -> zbus::Result<()>;
    /// Apps whose windows may show. This is not the ticket set: a ticket
    /// expiring only affects the next launch (docs/subsystems/window-management.md).
    fn window_states(&self) -> zbus::Result<Vec<String>>;
    /// Per-app unfocus relock delays, apps with the feature off left out.
    fn relock_seconds(&self) -> zbus::Result<Vec<(String, u64)>>;
    /// Change one option. An empty app id addresses the global settings.
    fn set_option(
        &self,
        app_id: &str,
        key: &str,
        value: zbus::zvariant::Value<'_>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    fn state_changed(&self, app_id: String, unlocked: bool) -> zbus::Result<()>;

    /// The screen unlocked and the window layer has to be reapplied.
    #[zbus(signal)]
    fn session_unlocked(&self) -> zbus::Result<()>;

    /// A global lock or screen lock invalidated the settings authorization.
    /// The window has to exit so its retained polkit decision cannot be
    /// reused after the desktop changes hands.
    #[zbus(signal)]
    fn settings_locked(&self) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_missing_method_interface_or_service_allows_a_fallback() {
        let call = zbus::Message::method_call("/Test", "ResolveApp")
            .unwrap()
            .build(&())
            .unwrap();
        let method_error = |name: &str| {
            let reply = zbus::Message::error(&call.header(), name)
                .unwrap()
                .build(&"test error")
                .unwrap();
            zbus::Error::from(reply)
        };
        for name in [
            "org.freedesktop.DBus.Error.UnknownMethod",
            "org.freedesktop.DBus.Error.UnknownInterface",
            "org.freedesktop.DBus.Error.ServiceUnknown",
            "org.freedesktop.DBus.Error.NameHasNoOwner",
        ] {
            assert!(method_unavailable(&method_error(name)), "{name}");
        }
        for name in [
            "org.freedesktop.DBus.Error.AccessDenied",
            "org.freedesktop.DBus.Error.NoReply",
            "org.freedesktop.DBus.Error.TimedOut",
        ] {
            assert!(!method_unavailable(&method_error(name)), "{name}");
        }
        assert!(!method_unavailable(&zbus::Error::InvalidReply));
    }
}
