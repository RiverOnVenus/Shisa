//! shisa-settings: the settings window. A Tauri shell around the system bus.
//!
//! Read commands open the system bus and build the proxy they need. Commands
//! that may need authentication share one connection for the window's
//! lifetime. polkit keeps an `auth_admin_keep` decision against the process,
//! not the connection, so the shared connection is not what makes consecutive
//! changes prompt once. It gives shisad a stable name for the `SettingsLocked`
//! signal, which makes the window exit when the desktop is locked and ends
//! that decision (docs/subsystems/authentication.md). Changes block here until
//! shisad answers, which the frontend shows as a busy overlay.

use base64::Engine;
use futures_util::StreamExt;
use shisa_core::dbus::{
    AppRow, BUS_NAME, DiagRow, LOCK_CONFIRMED, LOCK_NO_LAYER, LOCK_PARTIAL, LaunchRow, LockReport,
    ManagerProxy,
};
use shisa_core::i18n::{self, Language};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};
use zbus::MatchRule;
use zbus::fdo::NameOwnerChanged;
use zbus::message::Type;

type R<T> = Result<T, String>;

/// Localize the daemon's or zbus's English error for the window. The daemon
/// keeps one canonical wording on the bus, and this is where the settings
/// window turns it into the interface language.
fn err(e: impl std::fmt::Display) -> String {
    i18n::error_text(&e.to_string(), i18n::current())
}

/// The desktop's preferred colour scheme, asked of the XDG portal.
///
/// WebKitGTK follows GTK's dark setting, and KDE leaves GTK light even when
/// Plasma is dark, so a media query alone leaves this window light under a
/// dark desktop. The portal is where both desktops answer. The value is a
/// `u`: 0 no preference, 1 dark, 2 light.
mod appearance {
    use zbus::zvariant::OwnedValue;

    pub const NAMESPACE: &str = "org.freedesktop.appearance";
    pub const KEY: &str = "color-scheme";

    #[zbus::proxy(
        interface = "org.freedesktop.portal.Settings",
        default_service = "org.freedesktop.portal.Desktop",
        default_path = "/org/freedesktop/portal/desktop"
    )]
    pub trait Settings {
        fn read(&self, namespace: &str, key: &str) -> zbus::Result<OwnedValue>;

        #[zbus(signal)]
        fn setting_changed(
            &self,
            namespace: String,
            key: String,
            value: OwnedValue,
        ) -> zbus::Result<()>;
    }

    /// `Some(true)` dark, `Some(false)` light. `None` is no preference or no
    /// portal, and then the page keeps its media query.
    pub fn scheme(value: OwnedValue) -> Option<bool> {
        match u32::try_from(value).ok()? {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        }
    }

    /// One answer from the portal, `None` when it cannot be asked.
    pub async fn read() -> Option<bool> {
        let conn = zbus::Connection::session().await.ok()?;
        let settings = SettingsProxy::new(&conn).await.ok()?;
        scheme(settings.read(NAMESPACE, KEY).await.ok()?)
    }
}

/// Send the desktop's scheme to the page now and whenever it changes. The
/// page paints itself from the latest answer, so a switch of Plasma's colours
/// reaches an open window.
async fn follow_appearance(window: tauri::WebviewWindow) {
    use futures_util::future::{Either, select};
    use futures_util::{StreamExt, pin_mut};
    let (closed_tx, mut closed) = futures_channel::oneshot::channel();
    let closed_tx = Arc::new(Mutex::new(Some(closed_tx)));
    window.on_window_event({
        let closed_tx = closed_tx.clone();
        move |event| {
            if matches!(event, tauri::WindowEvent::Destroyed)
                && let Some(tx) = closed_tx.lock().unwrap().take()
            {
                let _ = tx.send(());
            }
        }
    });
    drop(closed_tx);
    let Ok(conn) = zbus::Connection::session().await else {
        return;
    };
    let Ok(settings) = appearance::SettingsProxy::new(&conn).await else {
        return;
    };
    let first = settings.read(appearance::NAMESPACE, appearance::KEY);
    pin_mut!(first);
    let first = match select(first, &mut closed).await {
        Either::Left((Ok(value), _)) => Some(value),
        Either::Left((Err(_), _)) => None,
        Either::Right(_) => return,
    };
    if let Some(value) = first {
        let _ = window.emit("color_scheme", appearance::scheme(value));
    }
    let changes = settings.receive_setting_changed();
    pin_mut!(changes);
    let mut changes = match select(changes, &mut closed).await {
        Either::Left((Ok(changes), _)) => changes,
        Either::Left((Err(_), _)) | Either::Right(_) => return,
    };
    loop {
        let next = changes.next();
        pin_mut!(next);
        let signal = match select(next, &mut closed).await {
            Either::Left((Some(signal), _)) => signal,
            Either::Left((None, _)) | Either::Right(_) => break,
        };
        let Ok(args) = signal.args() else {
            continue;
        };
        if args.namespace == appearance::NAMESPACE && args.key == appearance::KEY {
            let _ = window.emit("color_scheme", appearance::scheme(args.value));
        }
    }
}

/// The window asks once on mount, so an answer that came before the page was
/// listening is not lost.
#[tauri::command]
async fn color_scheme() -> R<Option<bool>> {
    Ok(appearance::read().await)
}

/// The interface language this window should render, as a tag the frontend
/// knows. Read before the window mounts, so the first paint is already right.
#[tauri::command]
fn ui_language() -> String {
    i18n::current().tag().to_string()
}

/// Save the language chosen in the settings window. This is a user
/// preference, so it needs no polkit authorization.
#[tauri::command]
fn set_ui_language(language: String) -> R<()> {
    let language = Language::from_tag(&language).ok_or("unknown language")?;
    i18n::save(language).map_err(err)
}

/// The version of this settings binary, to compare with the daemon's.
#[tauri::command]
fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Whether `shisa-agent` owns its session bus name. The tray and window
/// hiding live there, so the settings window can report them independently
/// of shisad. A missing session bus is not an error here: it simply means
/// the agent is unavailable.
#[tauri::command]
async fn agent_status() -> bool {
    let Ok(connection) = zbus::Connection::session().await else {
        return false;
    };
    connection
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "NameHasOwner",
            &(shisa_core::dbus::WINDOW_BUS_NAME,),
        )
        .await
        .ok()
        .and_then(|reply| reply.body().deserialize::<bool>().ok())
        .unwrap_or(false)
}

/// Open or focus the settings window from the restore panel. GTK and window
/// creation must run on the main thread, and the command handler may not be
/// there, so schedule the work and return.
#[tauri::command]
fn open_settings(app: tauri::AppHandle) -> R<()> {
    let main = app.clone();
    app.run_on_main_thread(move || {
        if let Err(error) = open_window(&main, false) {
            eprintln!("Shisa: {error}");
        }
    })
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_apps() -> R<Vec<AppRow>> {
    let conn = zbus::Connection::system().await.map_err(err)?;
    let manager = ManagerProxy::new(&conn).await.map_err(err)?;
    manager.list_apps().await.map_err(err)
}

#[tauri::command]
async fn list_installed() -> R<Vec<(String, String, String)>> {
    let conn = zbus::Connection::system().await.map_err(err)?;
    let manager = ManagerProxy::new(&conn).await.map_err(err)?;
    manager.list_installed().await.map_err(err)
}

#[tauri::command]
async fn app_config(app_id: String) -> R<(u64, bool, bool, u64)> {
    let conn = zbus::Connection::system().await.map_err(err)?;
    let manager = ManagerProxy::new(&conn).await.map_err(err)?;
    manager.app_config(&app_id).await.map_err(err)
}

#[tauri::command]
async fn global_config() -> R<(u64, bool, u64)> {
    let conn = zbus::Connection::system().await.map_err(err)?;
    let manager = ManagerProxy::new(&conn).await.map_err(err)?;
    manager.global_config().await.map_err(err)
}

/// The caller's recent launch decisions, for the activity page.
#[tauri::command]
async fn recent_launches() -> R<(Vec<LaunchRow>, u64, u64, u64)> {
    let conn = zbus::Connection::system().await.map_err(err)?;
    let manager = ManagerProxy::new(&conn).await.map_err(err)?;
    manager.recent_launches().await.map_err(err)
}

/// The daemon's own numbers, for the general page's diagnostics card.
#[tauri::command]
async fn diagnostics() -> R<DiagRow> {
    let conn = zbus::Connection::system().await.map_err(err)?;
    let manager = ManagerProxy::new(&conn).await.map_err(err)?;
    manager.diagnostics().await.map_err(err)
}

/// Authenticate for one app and leave a ticket, as `shisa unlock` does. Blocks
/// until the box answers.
#[tauri::command]
async fn unlock(bus: State<'_, Bus>, app_id: String) -> R<()> {
    let conn = bus.connection().await?;
    let manager = ManagerProxy::new(&conn).await.map_err(err)?;
    manager.unlock(&app_id).await.map_err(err)
}

#[tauri::command]
async fn resolve_app(app_id: String) -> R<(Vec<String>, Vec<String>)> {
    let conn = zbus::Connection::system().await.map_err(err)?;
    let manager = ManagerProxy::new(&conn).await.map_err(err)?;
    manager.resolve_app(&app_id).await.map_err(err)
}

#[tauri::command]
async fn set_protected(bus: State<'_, Bus>, app_id: String, on: bool) -> R<()> {
    let conn = bus.connection().await?;
    let manager = ManagerProxy::new(&conn).await.map_err(err)?;
    manager.set_protected(&app_id, on).await.map_err(err)
}

// One command per option, so the frontend cannot send a string where the
// daemon expects seconds. An empty app id addresses the global settings.

#[tauri::command]
async fn set_grace(bus: State<'_, Bus>, app_id: String, seconds: u64) -> R<()> {
    let conn = bus.connection().await?;
    let manager = ManagerProxy::new(&conn).await.map_err(err)?;
    manager
        .set_option(
            &app_id,
            "grace_seconds",
            zbus::zvariant::Value::from(seconds),
        )
        .await
        .map_err(err)
}

#[tauri::command]
async fn set_autostart(bus: State<'_, Bus>, app_id: String, allow: bool) -> R<()> {
    let conn = bus.connection().await?;
    let manager = ManagerProxy::new(&conn).await.map_err(err)?;
    manager
        .set_option(&app_id, "autostart", zbus::zvariant::Value::from(allow))
        .await
        .map_err(err)
}

#[tauri::command]
async fn set_relock(bus: State<'_, Bus>, app_id: String, seconds: u64) -> R<()> {
    let conn = bus.connection().await?;
    let manager = ManagerProxy::new(&conn).await.map_err(err)?;
    manager
        .set_option(
            &app_id,
            "relock_after_unfocus_seconds",
            zbus::zvariant::Value::from(seconds),
        )
        .await
        .map_err(err)
}

#[tauri::command]
async fn set_paths(bus: State<'_, Bus>, app_id: String, paths: Vec<String>) -> R<()> {
    let conn = bus.connection().await?;
    let manager = ManagerProxy::new(&conn).await.map_err(err)?;
    manager
        .set_option(&app_id, "paths", zbus::zvariant::Value::from(paths))
        .await
        .map_err(err)
}

#[tauri::command]
async fn set_clear_on_lock(bus: State<'_, Bus>, on: bool) -> R<()> {
    let conn = bus.connection().await?;
    let manager = ManagerProxy::new(&conn).await.map_err(err)?;
    manager
        .set_option("", "clear_on_screen_lock", zbus::zvariant::Value::from(on))
        .await
        .map_err(err)
}

#[tauri::command]
async fn set_prompt_timeout(bus: State<'_, Bus>, seconds: u64) -> R<()> {
    let conn = bus.connection().await?;
    let manager = ManagerProxy::new(&conn).await.map_err(err)?;
    manager
        .set_option(
            "",
            "prompt_timeout_seconds",
            zbus::zvariant::Value::from(seconds),
        )
        .await
        .map_err(err)
}

#[derive(serde::Serialize)]
struct MissedWindow {
    app: String,
    count: u32,
}

/// The window layer's answer for this lock. The panel shows the status and
/// counts instead of claiming a success that KWin did not confirm.
#[derive(serde::Serialize)]
struct LockResult {
    /// `confirmed`, `partial`, `unconfirmed`, or `no-window-layer`.
    status: &'static str,
    generation: u64,
    matched: u64,
    hidden: u64,
    missed: Vec<MissedWindow>,
}

impl LockResult {
    fn from_reply((status, generation, matched, hidden, missed): LockReport) -> Self {
        let status = match status {
            LOCK_CONFIRMED => "confirmed",
            LOCK_PARTIAL => "partial",
            LOCK_NO_LAYER => "no-window-layer",
            _ => "unconfirmed",
        };
        Self {
            status,
            generation,
            matched,
            hidden,
            missed: missed
                .into_iter()
                .map(|(app, count)| MissedWindow { app, count })
                .collect(),
        }
    }

    fn unconfirmed() -> Self {
        Self {
            status: "unconfirmed",
            generation: 0,
            matched: 0,
            hidden: 0,
            missed: Vec::new(),
        }
    }
}

#[tauri::command]
async fn lock(app_id: String) -> R<Option<LockResult>> {
    let conn = zbus::Connection::system().await.map_err(err)?;
    let manager = ManagerProxy::new(&conn).await.map_err(err)?;
    manager.lock(&app_id).await.map_err(err)?;
    if !app_id.is_empty() {
        return Ok(None);
    }
    Ok(Some(check_lock_windows().await))
}

/// The bus error names that mean the agent is absent or too old for the new
/// method. Other errors, including a timeout, must not trigger a second
/// check and a second notification.
fn unavailable(error: &zbus::Error) -> bool {
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

/// Ask the session agent for this lock's window report. A failed or older
/// agent call leaves the lock in force but the window state unconfirmed. The
/// method timeout bounds a check that is waiting on a hung KWin.
async fn check_lock_windows() -> LockResult {
    use shisa_core::dbus::{WINDOW_BUS_NAME, WINDOW_INTERFACE, WINDOW_PATH};
    let checked = async {
        let session = zbus::connection::Builder::session()?
            .method_timeout(std::time::Duration::from_secs(5))
            .build()
            .await?;
        let proxy =
            zbus::Proxy::new(&session, WINDOW_BUS_NAME, WINDOW_PATH, WINDOW_INTERFACE).await?;
        proxy
            .call::<_, _, LockReport>("VerifyLockReport", &())
            .await
    }
    .await;
    match checked {
        Ok(reply) => LockResult::from_reply(reply),
        Err(e) => {
            // An older agent only has VerifyLock. It still shows its own
            // notification, but this window has no report for the operation.
            if unavailable(&e) {
                let _ = verify_lock_legacy().await;
            }
            LockResult::unconfirmed()
        }
    }
}

async fn verify_lock_legacy() -> zbus::Result<()> {
    use shisa_core::dbus::{WINDOW_BUS_NAME, WINDOW_INTERFACE, WINDOW_PATH};
    let session = zbus::connection::Builder::session()?
        .method_timeout(std::time::Duration::from_secs(5))
        .build()
        .await?;
    let proxy = zbus::Proxy::new(&session, WINDOW_BUS_NAME, WINDOW_PATH, WINDOW_INTERFACE).await?;
    proxy.call::<_, _, ()>("VerifyLock", &()).await
}

/// The connection used for commands that may authenticate, and for the lock
/// watch.
///
/// polkit keeps the `auth_admin_keep` decision against the settings process,
/// not this connection. The shared connection gives shisad one stable name to
/// send `SettingsLocked` to, and the authenticated commands all use it so the
/// name that registered is the name that changes settings.
#[derive(Clone, Default)]
struct Bus(Arc<futures_util::lock::Mutex<Option<zbus::Connection>>>);

impl Bus {
    async fn connection(&self) -> R<zbus::Connection> {
        if let Some(conn) = self.0.lock().await.as_ref()
            && !conn.is_closed()
        {
            return Ok(conn.clone());
        }
        // A restarted system bus closes the old connection. Connect without
        // holding the lock so a slow bus does not block every command, then
        // prefer a connection another task stored meanwhile.
        let conn = zbus::Connection::system().await.map_err(err)?;
        let mut slot = self.0.lock().await;
        if let Some(existing) = slot.as_ref()
            && !existing.is_closed()
        {
            return Ok(existing.clone());
        }
        *slot = Some(conn.clone());
        Ok(conn)
    }
}

/// Exit when shisad reports a global or screen lock.
///
/// polkit keeps the settings authorization against this process, so exiting is
/// what ends it. The retry keeps the watch alive across a daemon start or a
/// system-bus restart, both of which invalidate the registration.
fn spawn_lock_watch(app: tauri::AppHandle, bus: Bus) {
    tauri::async_runtime::spawn(async move {
        let mut last: Option<String> = None;
        loop {
            match watch_lock(&app, &bus).await {
                Ok(true) => return,
                Ok(false) => last = None,
                Err(e) => {
                    if last.as_deref() != Some(e.as_str()) {
                        eprintln!("Shisa: lock watch: {e}");
                        last = Some(e);
                    }
                }
            }
            // A reconnect is rare, so a blocking sleep is not worth a timer
            // dependency.
            let _ = tauri::async_runtime::spawn_blocking(|| {
                std::thread::sleep(std::time::Duration::from_secs(2));
            })
            .await;
        }
    });
}

/// True when this `NameOwnerChanged` signal says the daemon instance this
/// window registered with is gone. An empty new owner means the name lost
/// every owner.
fn owner_replaced(name: &str, current: &str, signal_name: &str, new_owner: &str) -> bool {
    signal_name == name && new_owner != current
}

/// The bus signal that announces a new owner for Shisa's name.
fn owner_changes_rule() -> zbus::Result<MatchRule<'static>> {
    Ok(MatchRule::builder()
        .msg_type(Type::Signal)
        .sender("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .arg(0, BUS_NAME)?
        .build())
}

/// The current owner of a bus name, or `None` when it has none.
async fn name_owner(conn: &zbus::Connection, name: &str) -> Option<String> {
    let reply = conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetNameOwner",
            &(name,),
        )
        .await
        .ok()?;
    reply.body().deserialize::<String>().ok()
}

/// One registration and subscription. True means the app was told to exit:
/// either a lock arrived, or the daemon instance changed under it.
async fn watch_lock(app: &tauri::AppHandle, bus: &Bus) -> Result<bool, String> {
    let conn = bus.connection().await?;
    let manager = ManagerProxy::new(&conn).await.map_err(err)?;
    // Subscribe before the owner is read, so a change in between is still
    // delivered to the watcher below.
    let mut owners =
        zbus::MessageStream::for_match_rule(owner_changes_rule().map_err(err)?, &conn, None)
            .await
            .map_err(err)?;
    let Some(current) = name_owner(&conn, BUS_NAME).await else {
        return Ok(false);
    };
    // A lock between the generation snapshot and registration has no
    // destination to signal. The second snapshot catches that gap. An older
    // daemon has no generation method, so its registration keeps the previous
    // behavior instead of making the settings window unusable after upgrade.
    let generation = match manager.settings_generation().await {
        Ok(generation) => Some(generation),
        Err(error) if unavailable(&error) => None,
        Err(error) => return Err(err(error)),
    };
    let mut locked = manager.receive_settings_locked().await.map_err(err)?;
    manager.register_settings().await.map_err(err)?;
    if let Some(generation) = generation
        && manager.settings_generation().await.map_err(err)? != generation
    {
        app.exit(0);
        return Ok(true);
    }
    // Close the race between the owner lookup and the registration. A
    // registration that reached the previous instance is not useful. A
    // failed lookup is treated the same way: the window cannot prove which
    // instance holds its registration, and exiting is safer than staying
    // open unverified.
    if name_owner(&conn, BUS_NAME).await.as_deref() != Some(current.as_str()) {
        app.exit(0);
        return Ok(true);
    }
    // A restarted daemon starts with an empty settings registry, and the bus
    // keeps this connection's signal subscription alive across the owner
    // change. The retained polkit decision belongs to this process, so exit
    // rather than leave a window that a later lock cannot revoke.
    let owner_app = app.clone();
    let registered_owner = current.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(message) = owners.next().await {
            let Ok(message) = message else { continue };
            let Some(signal) = NameOwnerChanged::from_message(message) else {
                continue;
            };
            let Ok(args) = signal.args() else { continue };
            let new_owner = args.new_owner();
            let new_owner = match &**new_owner {
                Some(name) => name.as_str(),
                None => "",
            };
            if owner_replaced(BUS_NAME, &registered_owner, args.name().as_str(), new_owner) {
                owner_app.exit(0);
                return;
            }
        }
    });
    if locked.next().await.is_some() {
        app.exit(0);
        return Ok(true);
    }
    Ok(false)
}

/// Resolved icon files, as data URLs, keyed by theme name and pixel size. The
/// theme lookup happens once per key.
type IconCache = HashMap<(String, u32), Option<String>>;

#[derive(Clone, Default)]
struct Icons(Arc<Mutex<IconCache>>);

/// Resolving an icon walks several theme directories. Keep that off the IPC
/// thread: the first app list asks for one icon per row, and a few hundred
/// misses would otherwise freeze the window while it paints.
///
/// `px` is the physical pixel size the page will draw the icon at, from its
/// `devicePixelRatio`. The page sends it because this side cannot see how the
/// desktop scales the window.
#[tauri::command]
async fn icon(state: State<'_, Icons>, name: String, px: u32) -> R<Option<String>> {
    if name.is_empty() {
        return Ok(None);
    }
    let px = px.clamp(16, 512);
    if let Some(hit) = state.0.lock().unwrap().get(&(name.clone(), px)) {
        return Ok(hit.clone());
    }
    let cache = state.0.clone();
    let found = tauri::async_runtime::spawn_blocking(move || {
        // Two rows can ask for the same icon before either one finishes.
        if let Some(hit) = cache.lock().unwrap().get(&(name.clone(), px)) {
            return hit.clone();
        }
        let found = find_icon(&name, px);
        cache.lock().unwrap().insert((name, px), found.clone());
        found
    })
    .await
    .ok()
    .flatten();
    Ok(found)
}

/// The icon directories the session can see. They do not change while the
/// window is open, so the list is built once.
fn icon_roots() -> &'static [PathBuf] {
    static ROOTS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    ROOTS.get_or_init(|| {
        let mut bases = Vec::new();
        if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
            bases.push(PathBuf::from(data));
        } else if let Some(home) = std::env::var_os("HOME") {
            bases.push(PathBuf::from(home).join(".local/share"));
        }
        match std::env::var_os("XDG_DATA_DIRS") {
            Some(dirs) => bases.extend(std::env::split_paths(&dirs)),
            None => {
                bases.push(PathBuf::from("/usr/local/share"));
                bases.push(PathBuf::from("/usr/share"));
            }
        }
        // The daemon finds Flatpak applications in these exports, and the host
        // session's XDG_DATA_DIRS does not list them.
        if let Some(home) = std::env::var_os("HOME") {
            bases.push(PathBuf::from(home).join(".local/share/flatpak/exports/share"));
        }
        bases.push(PathBuf::from("/var/lib/flatpak/exports/share"));
        let mut roots: Vec<PathBuf> = Vec::new();
        for base in bases {
            let root = base.join("icons");
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        let system = PathBuf::from("/usr/share/icons");
        if !roots.contains(&system) {
            roots.push(system);
        }
        roots
    })
}

/// The icon theme KDE uses, so the window shows the same icons as the rest of
/// the desktop. Papirus and friends keep their own icons and only inherit
/// hicolor, so searching a fixed list of themes misses most applications.
fn current_theme() -> String {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return "breeze".into();
    };
    let Ok(text) = std::fs::read_to_string(home.join(".config/kdeglobals")) else {
        return "breeze".into();
    };
    kde_icon_theme(&text).unwrap_or_else(|| "breeze".into())
}

/// The `[Icons] Theme=` value. An unreadable or incomplete file means KDE's
/// default, so `None` keeps `current_theme`'s fallback in one place.
fn kde_icon_theme(text: &str) -> Option<String> {
    let mut in_icons = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_icons = line == "[Icons]";
            continue;
        }
        if in_icons
            && let Some(theme) = line.strip_prefix("Theme=")
            && !theme.is_empty()
        {
            return Some(theme.to_string());
        }
    }
    None
}

/// Themes named in the current theme's index.theme, in order.
fn inherited(theme: &str) -> Vec<String> {
    for root in icon_roots() {
        let Ok(text) = std::fs::read_to_string(root.join(theme).join("index.theme")) else {
            continue;
        };
        return inherited_themes(&text);
    }
    Vec::new()
}

fn inherited_themes(text: &str) -> Vec<String> {
    let mut in_icon_theme = false;
    let mut chain = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_icon_theme = line == "[Icon Theme]";
            continue;
        }
        if in_icon_theme && let Some(list) = line.strip_prefix("Inherits=") {
            for name in list.split(',') {
                let name = name.trim();
                if !name.is_empty() {
                    chain.push(name.to_string());
                }
            }
        }
    }
    chain
}

/// Raster sizes icon themes ship, ascending. `scalable` is tried before any of
/// them because vector art is sharp at every size.
const RASTER_SIZES: [u32; 10] = [16, 22, 24, 32, 48, 64, 96, 128, 256, 512];

/// Icon sizes to try, best first. A raster at least as large as the request is
/// drawn down and stays sharp, so those come first with the smallest winning
/// to keep the data URL short. Sizes below the request follow, largest first,
/// because they can only be stretched and the largest stretches least. This is
/// why a 16px PNG must not be picked for a 22px row on a scaled display.
fn size_order(px: u32) -> Vec<u32> {
    let (mut order, below): (Vec<u32>, Vec<u32>) =
        RASTER_SIZES.iter().copied().partition(|size| *size >= px);
    order.extend(below.into_iter().rev());
    order
}

/// The theme chain is the same for every icon in one run of the window.
/// Reading kdeglobals and every index.theme once keeps a long app list from
/// repeating that work for each row.
fn icon_themes() -> &'static [String] {
    static THEMES: OnceLock<Vec<String>> = OnceLock::new();
    THEMES.get_or_init(|| {
        let mut themes = vec![current_theme()];
        for parent in inherited(&themes[0]) {
            if !themes.contains(&parent) {
                themes.push(parent);
            }
        }
        for fallback in ["breeze", "hicolor"] {
            if !themes.iter().any(|t| t == fallback) {
                themes.push(fallback.to_string());
            }
        }
        themes
    })
}

fn find_icon(name: &str, px: u32) -> Option<String> {
    // Icon= may be an absolute path.
    if name.starts_with('/') {
        return data_url(std::path::Path::new(name));
    }

    let themes = icon_themes();
    for root in icon_roots() {
        // A relative path in Icon= is rooted at the icon directory.
        if name.contains('/')
            && let Some(found) = data_url(&root.join(name))
        {
            return Some(found);
        }
        for theme in themes {
            // A theme's scalable art is sharp at every size, so it wins over
            // any raster even when the raster matches the request exactly.
            for ext in ["svg", "png"] {
                let file = format!("{name}.{ext}");
                for path in [
                    root.join(theme).join("scalable").join("apps").join(&file),
                    root.join(theme).join("apps").join("scalable").join(&file),
                ] {
                    if let Some(found) = data_url(&path) {
                        return Some(found);
                    }
                }
            }
            for size in size_order(px) {
                for ext in ["svg", "png"] {
                    let file = format!("{name}.{ext}");
                    for path in [
                        root.join(theme)
                            .join("apps")
                            .join(size.to_string())
                            .join(&file),
                        root.join(theme)
                            .join(format!("{size}x{size}"))
                            .join("apps")
                            .join(&file),
                    ] {
                        if let Some(found) = data_url(&path) {
                            return Some(found);
                        }
                    }
                }
            }
        }
    }

    // Last resort: the flat pixmap directories.
    let mut pixmaps = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        pixmaps.push(PathBuf::from(home).join(".local/share/pixmaps"));
    }
    pixmaps.push(PathBuf::from("/usr/share/pixmaps"));
    for dir in pixmaps {
        for ext in ["svg", "png", "xpm"] {
            if let Some(found) = data_url(&dir.join(format!("{name}.{ext}"))) {
                return Some(found);
            }
        }
    }
    None
}

/// Icons are small. An `Icon=` path can be absolute or reach through a
/// symlink, so refuse a device or an unbounded file instead of reading it
/// into memory.
const MAX_ICON_BYTES: u64 = 4 << 20;

fn data_url(path: &std::path::Path) -> Option<String> {
    let mime = match path.extension().and_then(|e| e.to_str()) {
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("xpm") => "image/x-xpixmap",
        _ => return None,
    };
    let bytes = shisa_core::files::read_small_file(path, MAX_ICON_BYTES, |_, _| Ok(())).ok()?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    Some(format!("data:{mime};base64,{encoded}"))
}

#[zbus::proxy(
    interface = "io.github.RiverOnVenus.Shisa.Window",
    default_service = "io.github.RiverOnVenus.Shisa.Window",
    default_path = "/io/github/RiverOnVenus/Shisa/Window",
    gen_blocking = false
)]
trait WindowAgent {
    fn hidden_apps(&self) -> zbus::Result<std::collections::HashMap<String, u32>>;
    fn restore_window(&self, app: &str) -> zbus::Result<()>;
}

#[tauri::command]
async fn hidden_apps() -> R<std::collections::HashMap<String, u32>> {
    let connection = zbus::Connection::session().await.map_err(err)?;
    let agent = WindowAgentProxy::new(&connection).await.map_err(err)?;
    agent.hidden_apps().await.map_err(err)
}

#[tauri::command]
async fn restore_hidden(bus: State<'_, Bus>, app_id: String) -> R<()> {
    let connection = zbus::Connection::session().await.map_err(err)?;
    let agent = WindowAgentProxy::new(&connection).await.map_err(err)?;
    let apps = agent.hidden_apps().await.map_err(err)?;
    if apps.get(&app_id).copied().unwrap_or_default() == 0 {
        return Err(match i18n::current() {
            Language::ZhCn => "这个窗口已不在列表中".into(),
            Language::En => "this window is no longer in the list".into(),
        });
    }
    // An already allowed window needs no prompt. Otherwise a valid ticket
    // restores the permission, and only an app without one waits for polkit,
    // so a cancelled prompt reaches the panel as an error instead of
    // silently closing it. An older shisad has no Unhide and falls through
    // to authentication.
    let system = bus.connection().await?;
    let manager = ManagerProxy::new(&system).await.map_err(err)?;
    if !manager
        .window_states()
        .await
        .map_err(err)?
        .contains(&app_id)
        && !manager.unhide(&app_id).await.unwrap_or(false)
    {
        manager.unlock(&app_id).await.map_err(err)?;
    }
    agent.restore_window(&app_id).await.map_err(err)
}

/// Give the frameless Wayland window the GTK client-side decoration's shadow
/// and resize edges.
///
/// tao makes a Wayland window without decorations client-decorated by giving
/// it an empty titlebar, so KWin draws no server-side frame on it. GTK then
/// has `decorated` off, and its `get_shadow_width()` returns zero in that
/// state, so GTK paints no shadow either. Turning `decorated` back on keeps
/// the empty titlebar and lets GTK render the theme's decoration shadow in
/// the margin it reserves for resize grips, as its other windows do. Under
/// X11 tao installs no titlebar, and this would ask the window manager for
/// its decorations, so it is Wayland-only.
///
/// Adwaita rounds only the decoration's top corners (`border-radius: 8px
/// 8px 0 0`), and the shadow follows that shape, which makes a frameless
/// square page look rounded above and square below. The class-scoped rule
/// squares every corner while keeping the theme's own shadow colour, blur
/// and resize margin. The class applies to both the settings and restore
/// windows.
#[cfg(target_os = "linux")]
fn add_csd_shadow(window: &tauri::WebviewWindow) {
    use gtk::prelude::*;
    let Ok(gtk_window) = window.gtk_window() else {
        return;
    };
    if !gtk_window.display().backend().is_wayland() {
        return;
    }
    gtk_window
        .style_context()
        .add_class("shisa-flat-decoration");
    if let Some(screen) = gtk::prelude::WidgetExt::screen(&gtk_window) {
        let provider = gtk::CssProvider::new();
        if let Err(error) =
            provider.load_from_data(b".shisa-flat-decoration decoration { border-radius: 0; }")
        {
            eprintln!("Shisa: flat decoration rule: {error}");
        } else {
            gtk::StyleContext::add_provider_for_screen(
                &screen,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
    }
    gtk_window.set_decorated(true);
}

fn open_window(app: &tauri::AppHandle, restore: bool) -> Result<(), Box<dyn std::error::Error>> {
    let label = if restore { "restore" } else { "main" };
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.unminimize();
        let _ = window.set_focus();
        return Ok(());
    }
    // The runtime directory is per-user and private. A fixed fallback under
    // /tmp is not, and another account could pre-create or poison it.
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set")
        })?;
    let data = runtime.join(if restore {
        "shisa-restore"
    } else {
        "shisa-settings"
    });
    std::fs::create_dir_all(&data)?;
    let builder = WebviewWindowBuilder::new(app, label, WebviewUrl::App("index.html".into()))
        .data_directory(data);
    let language = i18n::current();
    let window = if restore {
        builder
            .title(match language {
                Language::ZhCn => "恢复窗口",
                Language::En => "Restore windows",
            })
            .inner_size(384.0, 400.0)
            .resizable(false)
            .decorations(false)
            .transparent(true)
            .center()
            .build()?
    } else {
        builder
            .title(match language {
                Language::ZhCn => "Shisa 设置",
                Language::En => "Shisa Settings",
            })
            .inner_size(940.0, 620.0)
            .min_inner_size(760.0, 520.0)
            .decorations(false)
            .transparent(true)
            .build()?
    };
    // Both windows use the same frameless page and GTK decoration shadow, so
    // the restore panel does not fall back to a native titlebar.
    let _ = window.set_background_color(Some(tauri::window::Color(0, 0, 0, 0)));
    #[cfg(target_os = "linux")]
    add_csd_shadow(&window);
    tauri::async_runtime::spawn(follow_appearance(window));
    Ok(())
}

fn main() {
    // GTK names the Wayland surface after the program name, and the compositor
    // looks the desktop file up by it. Without this the titlebar shows a
    // generic icon instead of the Shisa one.
    glib::set_prgname(Some("io.github.RiverOnVenus.Shisa"));
    let bus = Bus::default();
    let lock_bus = bus.clone();
    tauri::Builder::default()
        // A second start brings the existing window forward instead of opening
        // another one. The plugin calls back from its D-Bus thread, and GTK
        // may only be used from the main thread (the CSD shadow creates a CSS
        // provider), so window creation is scheduled there.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            let handle = app.clone();
            let restore = args.iter().any(|arg| arg == "--restore-windows");
            if let Err(error) = app.run_on_main_thread(move || {
                if let Err(error) = open_window(&handle, restore) {
                    eprintln!("Shisa: {error}");
                }
            }) {
                eprintln!("Shisa: {error}");
            }
        }))
        .setup(move |app| {
            spawn_lock_watch(app.handle().clone(), lock_bus);
            open_window(
                app.handle(),
                std::env::args_os().any(|arg| arg == "--restore-windows"),
            )
        })
        .manage(Icons::default())
        .manage(bus)
        .invoke_handler(tauri::generate_handler![
            list_apps,
            list_installed,
            recent_launches,
            diagnostics,
            unlock,
            hidden_apps,
            restore_hidden,
            app_config,
            global_config,
            resolve_app,
            set_protected,
            set_grace,
            set_autostart,
            set_relock,
            set_paths,
            set_clear_on_lock,
            set_prompt_timeout,
            lock,
            icon,
            color_scheme,
            ui_language,
            set_ui_language,
            app_version,
            agent_status,
            open_settings
        ])
        .run(tauri::generate_context!())
        .expect("shisa-settings failed to start");
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::{OwnedValue, Value};

    fn owned(value: Value<'_>) -> OwnedValue {
        value.try_into().unwrap()
    }

    #[test]
    fn a_new_daemon_owner_invalidates_the_registration() {
        assert!(owner_replaced("shisa", "old", "shisa", "new"));
        // The daemon stopped and the name has no owner yet.
        assert!(owner_replaced("shisa", "old", "shisa", ""));
        assert!(!owner_replaced("shisa", "old", "shisa", "old"));
        assert!(!owner_replaced("shisa", "old", "other", "new"));
    }

    #[test]
    fn a_lock_report_maps_the_status_and_counts() {
        let full = LockResult::from_reply((LOCK_CONFIRMED, 7, 2, 2, Vec::new()));
        assert_eq!(full.status, "confirmed");
        assert_eq!((full.generation, full.matched, full.hidden), (7, 2, 2));
        assert!(full.missed.is_empty());

        let partial =
            LockResult::from_reply((LOCK_PARTIAL, 8, 3, 2, vec![("org.kde.kate".into(), 1)]));
        assert_eq!(partial.status, "partial");
        assert_eq!(partial.missed[0].app, "org.kde.kate");
        assert_eq!(partial.missed[0].count, 1);

        assert_eq!(
            LockResult::from_reply((LOCK_NO_LAYER, 0, 0, 0, Vec::new())).status,
            "no-window-layer"
        );
        // A status from a newer agent is not read as a confirmation.
        assert_eq!(
            LockResult::from_reply((99, 0, 0, 0, Vec::new())).status,
            "unconfirmed"
        );
        assert_eq!(LockResult::unconfirmed().status, "unconfirmed");
    }

    #[test]
    fn an_icon_path_must_be_a_small_regular_file() {
        let dir = std::env::temp_dir().join(format!("shisa-icon-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let small = dir.join("small.png");
        std::fs::write(&small, b"\x89PNG").unwrap();
        assert!(data_url(&small).is_some());
        let large = dir.join("large.png");
        let file = std::fs::File::create(&large).unwrap();
        file.set_len(MAX_ICON_BYTES + 1).unwrap();
        assert!(data_url(&large).is_none());
        let zero = dir.join("zero.png");
        std::os::unix::fs::symlink("/dev/zero", &zero).unwrap();
        assert!(data_url(&zero).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_small_raster_is_not_chosen_for_a_larger_box() {
        // A 1.25x desktop draws the 22px row at 28 physical pixels. The 16px
        // raster used to come first and was stretched, which is the blur.
        assert_eq!(size_order(28)[0], 32);
        assert_eq!(size_order(22)[0], 22);
        assert_eq!(size_order(20)[0], 22);
        assert_eq!(size_order(44)[0], 48);
        // Nothing large enough leaves the largest raster first: it stretches
        // the least.
        assert_eq!(size_order(900)[0], 512);
        // Every size is still tried, so a theme that ships only one resolves.
        let mut order = size_order(28);
        order.sort_unstable();
        assert_eq!(order, RASTER_SIZES);
    }

    #[test]
    fn every_manifest_names_the_same_release_version() {
        // The settings window compares this crate's version with shisad's.
        // A release that bumps one manifest and not the others would leave a
        // permanent mismatch notice on a correct install.
        let version = env!("CARGO_PKG_VERSION");
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let repo = manifest.join("../../..");
        let workspace = std::fs::read_to_string(repo.join("Cargo.toml")).unwrap();
        assert!(
            workspace.contains(&format!("version = \"{version}\"")),
            "the workspace Cargo.toml does not name {version}"
        );
        let package = std::fs::read_to_string(manifest.join("../package.json")).unwrap();
        assert!(
            package.contains(&format!("\"version\": \"{version}\"")),
            "package.json does not name {version}"
        );
        let tauri = std::fs::read_to_string(manifest.join("tauri.conf.json")).unwrap();
        assert!(
            tauri.contains(&format!("\"version\": \"{version}\"")),
            "tauri.conf.json does not name {version}"
        );
    }

    #[test]
    fn the_portal_scheme_maps_to_dark_and_light() {
        assert_eq!(appearance::scheme(owned(Value::from(1u32))), Some(true));
        assert_eq!(appearance::scheme(owned(Value::from(2u32))), Some(false));
        assert_eq!(appearance::scheme(owned(Value::from(0u32))), None);
        assert_eq!(appearance::scheme(owned(Value::from(3u32))), None);
        // A value of another type is no preference, not a panic.
        assert_eq!(appearance::scheme(owned(Value::from("dark"))), None);
    }

    #[test]
    fn the_theme_comes_from_the_icons_group() {
        assert_eq!(
            kde_icon_theme("[General]\nTheme=x\n[Icons]\nTheme=Breeze\n"),
            Some("Breeze".into())
        );
        assert_eq!(kde_icon_theme("[Other]\nTheme=Breeze\n"), None);
        assert_eq!(kde_icon_theme("[Icons]\nTheme=\n"), None);
        assert_eq!(kde_icon_theme(""), None);
    }

    #[test]
    fn only_the_icon_theme_group_names_inherited_themes() {
        assert_eq!(
            inherited_themes("[Icon Theme]\nInherits=hicolor, breeze,  ,Papirus\n"),
            vec!["hicolor", "breeze", "Papirus"]
        );
        assert_eq!(
            inherited_themes("[Other]\nInherits=hicolor\n"),
            Vec::<String>::new()
        );
        assert_eq!(inherited_themes(""), Vec::<String>::new());
    }
}
