//! Names and the interface shared by shisad, the session agent and the CLI.
//!
//! shisad owns the name on the system bus. The agent calls `RegisterAgent` so
//! state changes reach it. `ListApps` and `Lock` act on the calling user
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
}
