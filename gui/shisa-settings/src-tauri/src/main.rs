//! shisa-settings: the settings window. A Tauri shell around the system bus.
//!
//! Every command connects to the system bus and builds the proxy it needs.
//! zbus does not reuse the connection between calls, but the window is not hot
//! enough to justify keeping one around. Changes that need authentication
//! block here until shisad answers, which the frontend shows as a busy
//! overlay.

use base64::Engine;
use shisa_core::dbus::{AppRow, DiagRow, LaunchRow, ManagerProxy};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

type R<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
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
    use futures_util::StreamExt;
    let Ok(conn) = zbus::Connection::session().await else {
        return;
    };
    let Ok(settings) = appearance::SettingsProxy::new(&conn).await else {
        return;
    };
    if let Ok(value) = settings.read(appearance::NAMESPACE, appearance::KEY).await {
        let _ = window.emit("color_scheme", appearance::scheme(value));
    }
    let Ok(mut changes) = settings.receive_setting_changed().await else {
        return;
    };
    while let Some(signal) = changes.next().await {
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
async fn unlock(app_id: String) -> R<()> {
    let conn = zbus::Connection::system().await.map_err(err)?;
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
async fn set_protected(app_id: String, on: bool) -> R<()> {
    let conn = zbus::Connection::system().await.map_err(err)?;
    let manager = ManagerProxy::new(&conn).await.map_err(err)?;
    manager.set_protected(&app_id, on).await.map_err(err)
}

// One command per option, so the frontend cannot send a string where the
// daemon expects seconds. An empty app id addresses the global settings.

#[tauri::command]
async fn set_grace(app_id: String, seconds: u64) -> R<()> {
    let conn = zbus::Connection::system().await.map_err(err)?;
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
async fn set_autostart(app_id: String, allow: bool) -> R<()> {
    let conn = zbus::Connection::system().await.map_err(err)?;
    let manager = ManagerProxy::new(&conn).await.map_err(err)?;
    manager
        .set_option(&app_id, "autostart", zbus::zvariant::Value::from(allow))
        .await
        .map_err(err)
}

#[tauri::command]
async fn set_relock(app_id: String, seconds: u64) -> R<()> {
    let conn = zbus::Connection::system().await.map_err(err)?;
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
async fn set_paths(app_id: String, paths: Vec<String>) -> R<()> {
    let conn = zbus::Connection::system().await.map_err(err)?;
    let manager = ManagerProxy::new(&conn).await.map_err(err)?;
    manager
        .set_option(&app_id, "paths", zbus::zvariant::Value::from(paths))
        .await
        .map_err(err)
}

#[tauri::command]
async fn set_clear_on_lock(on: bool) -> R<()> {
    let conn = zbus::Connection::system().await.map_err(err)?;
    let manager = ManagerProxy::new(&conn).await.map_err(err)?;
    manager
        .set_option("", "clear_on_screen_lock", zbus::zvariant::Value::from(on))
        .await
        .map_err(err)
}

#[tauri::command]
async fn set_prompt_timeout(seconds: u64) -> R<()> {
    let conn = zbus::Connection::system().await.map_err(err)?;
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

#[tauri::command]
async fn lock(app_id: String) -> R<()> {
    let conn = zbus::Connection::system().await.map_err(err)?;
    let manager = ManagerProxy::new(&conn).await.map_err(err)?;
    manager.lock(&app_id).await.map_err(err)?;
    if app_id.is_empty() {
        use shisa_core::dbus::{WINDOW_BUS_NAME, WINDOW_INTERFACE, WINDOW_PATH};
        let check = async {
            let session = zbus::Connection::session().await?;
            let proxy =
                zbus::Proxy::new(&session, WINDOW_BUS_NAME, WINDOW_PATH, WINDOW_INTERFACE).await?;
            proxy.call::<_, _, ()>("VerifyLock", &()).await
        }
        .await;
        check.map_err(|e| format!("启动已上锁，窗口状态未确认：{e}"))?;
    }
    Ok(())
}

/// Resolved icon files, as data URLs. The theme lookup happens once per name.
#[derive(Clone, Default)]
struct Icons(Arc<Mutex<HashMap<String, Option<String>>>>);

/// Resolving an icon walks several theme directories. Keep that off the IPC
/// thread: the first app list asks for one icon per row, and a few hundred
/// misses would otherwise freeze the window while it paints.
#[tauri::command]
async fn icon(state: State<'_, Icons>, name: String) -> R<Option<String>> {
    if name.is_empty() {
        return Ok(None);
    }
    if let Some(hit) = state.0.lock().unwrap().get(&name) {
        return Ok(hit.clone());
    }
    let cache = state.0.clone();
    let found = tauri::async_runtime::spawn_blocking(move || {
        // Two rows can ask for the same icon before either one finishes.
        if let Some(hit) = cache.lock().unwrap().get(&name) {
            return hit.clone();
        }
        let found = find_icon(&name);
        cache.lock().unwrap().insert(name, found.clone());
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
            return theme.to_string();
        }
    }
    "breeze".into()
}

/// Themes named in the current theme's index.theme, in order.
fn inherited(theme: &str) -> Vec<String> {
    for root in icon_roots() {
        let Ok(text) = std::fs::read_to_string(root.join(theme).join("index.theme")) else {
            continue;
        };
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
        return chain;
    }
    Vec::new()
}

/// Size order for icon lookup. Rows draw icons at 16 and 22px, so the small
/// rasters come first: a 512px PNG would be a hundred times the pixels and
/// bytes for an image that is downscaled anyway. Larger sizes and scalable
/// art remain as fallbacks for themes that ship nothing smaller.
const SIZES: [&str; 11] = [
    "16", "22", "24", "32", "48", "64", "96", "128", "256", "512", "scalable",
];

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

fn find_icon(name: &str) -> Option<String> {
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
            for size in SIZES {
                for ext in ["svg", "png"] {
                    let file = format!("{name}.{ext}");
                    let mut candidates = vec![root.join(theme).join("apps").join(size).join(&file)];
                    if size == "scalable" {
                        candidates.push(root.join(theme).join("scalable").join("apps").join(&file));
                    } else {
                        candidates.push(
                            root.join(theme)
                                .join(format!("{size}x{size}"))
                                .join("apps")
                                .join(&file),
                        );
                    }
                    for path in candidates {
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

fn data_url(path: &std::path::Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let mime = match path.extension().and_then(|e| e.to_str()) {
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("xpm") => "image/x-xpixmap",
        _ => return None,
    };
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
async fn restore_hidden(app_id: String) -> R<()> {
    let connection = zbus::Connection::session().await.map_err(err)?;
    let agent = WindowAgentProxy::new(&connection).await.map_err(err)?;
    let apps = agent.hidden_apps().await.map_err(err)?;
    if apps.get(&app_id).copied().unwrap_or_default() == 0 {
        return Err("这个窗口已不在列表中".into());
    }
    let system = zbus::Connection::system().await.map_err(err)?;
    let manager = ManagerProxy::new(&system).await.map_err(err)?;
    if !manager.window_states().await.map_err(err)?.contains(&app_id) {
        manager.unlock(&app_id).await.map_err(err)?;
    }
    agent.restore_window(&app_id).await.map_err(err)
}

fn open_window(app: &tauri::AppHandle, restore: bool) -> Result<(), Box<dyn std::error::Error>> {
    let label = if restore { "restore" } else { "main" };
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.unminimize();
        let _ = window.set_focus();
        return Ok(());
    }
    let data = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(if restore { "shisa-restore" } else { "shisa-settings" });
    std::fs::create_dir_all(&data)?;
    let builder = WebviewWindowBuilder::new(app, label, WebviewUrl::App("index.html".into()))
        .data_directory(data);
    let window = if restore {
        builder.title("恢复窗口").inner_size(384.0, 400.0).resizable(false).center().build()?
    } else {
        builder.title("Shisa 设置")
            .inner_size(940.0, 620.0)
            .min_inner_size(760.0, 520.0)
            .decorations(false)
            .transparent(true)
            .build()?
    };
    if !restore {
        let _ = window.set_background_color(Some(tauri::window::Color(0, 0, 0, 0)));
    }
    tauri::async_runtime::spawn(follow_appearance(window));
    Ok(())
}

fn main() {
    // GTK names the Wayland surface after the program name, and the compositor
    // looks the desktop file up by it. Without this the titlebar shows a
    // generic icon instead of the Shisa one.
    glib::set_prgname(Some("io.github.RiverOnVenus.Shisa"));
    tauri::Builder::default()
        // A second start brings the existing window forward instead of opening
        // another one.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if let Err(error) = open_window(app, args.iter().any(|arg| arg == "--restore-windows")) {
                eprintln!("Shisa: {error}");
            }
        }))
        .setup(|app| open_window(app.handle(), std::env::args_os().any(|arg| arg == "--restore-windows")))
        .manage(Icons::default())
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
            color_scheme
        ])
        .run(tauri::generate_context!())
        .expect("shisa-settings failed to start");
}
