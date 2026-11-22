//! shisa-agent: the tray icon.
//!
//! Registers with shisad on the system bus so state changes reach this user,
//! and shows whether shisad is reachable. Authentication no longer goes
//! through this process: shisad asks polkit, and the desktop's own agent
//! shows the dialog.
//!
//! The tray lets the user drop tickets and open the settings window.
//! When shisad restarts or goes away the agent keeps running, marks itself
//! offline and registers again.

use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::{MenuItem, StandardItem};
use ksni::{Category, Status, Tray};
use shisa_core::dbus::{BUS_NAME, INTERFACE, ManagerProxyBlocking, WINDOW_BUS_NAME, WINDOW_PATH};
use shisa_core::i18n::{self, Language};
use std::collections::HashMap;
use std::env;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};
use zbus::MatchRule;
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::message::Type;

mod window;

use window::WindowReport;

/// What a menu entry asks the worker thread to do.
#[derive(Clone)]
enum Action {
    LockAll,
    VerifyLock,
    Unlock(String),
    RestoreIfTicketed(String),
    HiddenWindows((u64, u64), HashMap<String, u32>),
    OpenSettings,
    OpenRestore,
}

const OFFLINE_ICON: &str = "shisa-tray-offline-symbolic";
/// Used while the script reports at least one concealed window, so the
/// owner can see at a glance that the desktop is still locked.
const LOCKED_ICON: &str = "shisa-tray-locked-symbolic";
const NOTIFY_SERVICE: &str = "org.freedesktop.Notifications";
/// How long the desktop keeps a lock result notification on screen. A
/// result that still needs attention stays longer.
const NOTIFY_TIMEOUT_MS: i32 = 5_000;
const NOTIFY_PROBLEM_TIMEOUT_MS: i32 = 8_000;

struct TrayState {
    protected_count: usize,
    hidden_windows: ((u64, u64), HashMap<String, u32>),
    last_lock: Option<String>,
    offline: bool,
    /// The interface language, re-read while the tray runs.
    language: Language,
    actions: Sender<Action>,
}

impl TrayState {
    fn hidden_count(&self) -> u64 {
        self.hidden_windows
            .1
            .values()
            .map(|&count| u64::from(count))
            .sum()
    }
}

/// Pick between the two texts for the interface language.
fn text(language: Language, zh: &'static str, en: &'static str) -> &'static str {
    match language {
        Language::ZhCn => zh,
        Language::En => en,
    }
}

/// Follow the language the settings window saves. The preference is a small
/// file, so re-reading it every couple of seconds is cheaper than a watcher
/// and still makes a change visible without restarting the tray.
fn watch_language(handle: Handle<TrayState>) -> ! {
    let mut shown = i18n::current();
    loop {
        thread::sleep(Duration::from_secs(2));
        let now = i18n::current();
        if now != shown {
            shown = now;
            let _ = handle.update(|tray| tray.language = now);
        }
    }
}

impl Tray for TrayState {
    fn id(&self) -> String {
        "shisa-agent".into()
    }

    fn title(&self) -> String {
        "Shisa".into()
    }

    fn category(&self) -> Category {
        Category::SystemServices
    }

    fn status(&self) -> Status {
        if self.offline {
            Status::NeedsAttention
        } else {
            Status::Active
        }
    }

    fn icon_name(&self) -> String {
        if self.offline {
            OFFLINE_ICON
        } else if self.hidden_count() > 0 {
            LOCKED_ICON
        } else {
            "shisa-tray-symbolic"
        }
        .into()
    }

    fn attention_icon_name(&self) -> String {
        OFFLINE_ICON.into()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let mut description = if self.offline {
            text(
                self.language,
                "shisad 不在线，保护暂停",
                "shisad is offline, protection paused",
            )
            .to_string()
        } else {
            match (self.protected_count, self.language) {
                (1, Language::ZhCn) => "1 个应用受保护".to_string(),
                (count, Language::ZhCn) => format!("{count} 个应用受保护"),
                (1, Language::En) => "1 app protected".to_string(),
                (count, Language::En) => format!("{count} apps protected"),
            }
        };
        if let Some(last_lock) = &self.last_lock {
            description.push_str(&match self.language {
                Language::ZhCn => format!("\n上次上锁：{last_lock}"),
                Language::En => format!("\nLast lock: {last_lock}"),
            });
        }
        let hidden = self.hidden_count();
        if hidden > 0 {
            description.push_str(&match (hidden, self.language) {
                (1, Language::ZhCn) => "\nShisa 已隐藏 1 个窗口".to_string(),
                (hidden, Language::ZhCn) => format!("\nShisa 已隐藏 {hidden} 个窗口"),
                (1, Language::En) => "\nShisa hid 1 window".to_string(),
                (hidden, Language::En) => format!("\nShisa hid {hidden} windows"),
            });
        }
        ksni::ToolTip {
            icon_name: self.icon_name(),
            title: "Shisa".into(),
            description,
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.actions.send(Action::OpenRestore);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        use ksni::menu::MenuItem::Separator;
        let send = |action: Action| -> Box<dyn Fn(&mut TrayState) + Send> {
            let actions = self.actions.clone();
            Box::new(move |_this: &mut TrayState| {
                let _ = actions.send(action.clone());
            })
        };
        vec![
            StandardItem {
                label: text(self.language, "全部上锁", "Lock all").into(),
                enabled: !self.offline,
                activate: send(Action::LockAll),
                ..Default::default()
            }
            .into(),
            Separator,
            StandardItem {
                label: text(self.language, "打开设置", "Open settings").into(),
                activate: send(Action::OpenSettings),
                ..Default::default()
            }
            .into(),
        ]
    }
}

fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .without_time()
        .init();

    let (actions_tx, actions_rx) = mpsc::channel();
    // The gate is filled once the session connection is up. The action
    // thread uses it to bring windows back after an unlock and to bring an
    // already open settings window to the front. The tracker is shared with
    // the unix socket service, the refresh and the tray's unlock.
    let gate: Arc<Mutex<Option<window::Gate>>> = Arc::default();
    let tracker: Arc<Mutex<window::Tracker>> = Arc::default();
    let tray = TrayState {
        protected_count: 0,
        hidden_windows: Default::default(),
        last_lock: None,
        offline: true,
        language: i18n::current(),
        actions: actions_tx.clone(),
    };
    // The user unit starts with the graphical session, which can precede
    // plasmashell registering the StatusNotifierWatcher. Treat a missing
    // watcher as offline and let ksni register once it appears, rather than
    // failing the process on that startup race.
    let handle = match tray.assume_sni_available(true).spawn() {
        Ok(handle) => handle,
        Err(e) => {
            error!("tray: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    {
        let handle = handle.clone();
        thread::Builder::new()
            .name("language".into())
            .spawn(move || watch_language(handle))
            .expect("language thread");
    }
    {
        let gate = gate.clone();
        let handle = handle.clone();
        let dnd_hinted = Arc::new(AtomicBool::new(false));
        thread::Builder::new()
            .name("actions".into())
            .spawn(move || action_loop(actions_rx, &gate, &handle, &dnd_hinted))
            .expect("actions thread");
    }

    let conn = match Connection::system() {
        Ok(conn) => conn,
        Err(e) => {
            error!("system bus: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    // The session connection that serves the focus reporter has to stay
    // alive. KWin scripting runs on a separate session connection: mixing a
    // blocking script call and the object server on one connection leaves a
    // freshly loaded script's signal handlers unconnected.
    let ready = Arc::new(window::Ready::new());
    let service = window_service(tracker.clone(), ready.clone(), actions_tx);
    if service.is_some()
        && let Ok(kwin) = Connection::session()
    {
        *gate.lock().unwrap() = window::Gate::new(kwin, ready);
    }
    let _session = service;

    // A window that lost focus longer than its app's delay is hidden
    // without dropping the app's ticket. The timer only asks shisad to hide
    // it; the StateChanged that follows makes the gate hide it.
    {
        let conn = conn.clone();
        let tracker = tracker.clone();
        thread::Builder::new()
            .name("relock".into())
            .spawn(move || relock_loop(&conn, &tracker))
            .expect("relock thread");
    }

    // One connection carries everything: the registration, the signals shisad
    // sends to this unique name, and the checks on shisad's bus name.
    {
        let conn = conn.clone();
        let handle = handle.clone();
        let gate = gate.clone();
        let tracker = tracker.clone();
        thread::Builder::new()
            .name("signals".into())
            .spawn(move || signal_loop(&conn, &handle, &gate, &tracker))
            .expect("signals thread");
    }
    supervise(&conn, &handle, &gate, &tracker);
}

/// Claim the session name and serve the focus reporter. None when the
/// session bus is not available.
fn window_service(
    tracker: Arc<Mutex<window::Tracker>>,
    ready: Arc<window::Ready>,
    actions: Sender<Action>,
) -> Option<Connection> {
    let service = window::WindowService::new(tracker, ready, actions);
    match zbus::blocking::connection::Builder::session()
        .and_then(|builder| builder.name(WINDOW_BUS_NAME))
        .and_then(|builder| builder.serve_at(WINDOW_PATH, service))
        .and_then(|builder| builder.build())
    {
        Ok(conn) => Some(conn),
        Err(e) => {
            warn!("window gate: session bus: {e}");
            None
        }
    }
}

/// The bus says when shisad's name gets or loses its owner.
fn owner_changes(conn: &Connection) -> zbus::Result<MessageIterator> {
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .sender("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .arg(0, BUS_NAME)?
        .build();
    MessageIterator::for_match_rule(rule, conn, None)
}

/// Register whenever shisad appears, and say so when it goes away. The bus
/// announces both, so the loop sleeps in between. Without that signal it
/// falls back to asking every few seconds.
fn supervise(
    conn: &Connection,
    handle: &Handle<TrayState>,
    gate: &Arc<Mutex<Option<window::Gate>>>,
    tracker: &Arc<Mutex<window::Tracker>>,
) -> ! {
    let (wake_tx, wake_rx) = mpsc::channel::<()>();
    let subscribed = match owner_changes(conn) {
        Ok(stream) => thread::Builder::new()
            .name("owner".into())
            .spawn(move || {
                for message in stream {
                    if message.is_err() || wake_tx.send(()).is_err() {
                        break;
                    }
                }
            })
            .is_ok(),
        Err(e) => {
            warn!("cannot watch shisad's bus name, polling instead: {e}");
            false
        }
    };
    // A registration is repeated now and then in case shisad lost it.
    let mut interval = if subscribed {
        Duration::from_secs(60)
    } else {
        Duration::from_secs(3)
    };
    let mut online = false;
    loop {
        // Each pass follows an owner change or the timer, and registers
        // again: a restarted shisad starts with an empty registry.
        let present = name_has_owner(conn, BUS_NAME);
        if present {
            match ManagerProxyBlocking::new(conn).and_then(|manager| manager.register_agent()) {
                Ok(()) => {
                    if !online {
                        info!("registered with shisad");
                        online = true;
                        let _ = handle.update(|tray| tray.offline = false);
                    }
                    if let Ok(manager) = ManagerProxyBlocking::new(conn) {
                        refresh(handle, &manager, gate, tracker);
                    }
                }
                Err(e) => warn!("register: {e}"),
            }
        } else if !present && online {
            info!("shisad is gone");
            online = false;
            let _ = handle.update(|tray| tray.offline = true);
        }
        match wake_rx.recv_timeout(interval) {
            Ok(()) => {}
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                interval = Duration::from_secs(3);
                thread::sleep(interval);
            }
        }
    }
}

fn name_has_owner(conn: &Connection, name: &str) -> bool {
    conn.call_method(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        Some("org.freedesktop.DBus"),
        "NameHasOwner",
        &(name,),
    )
    .ok()
    .and_then(|reply| reply.body().deserialize::<bool>().ok())
    .unwrap_or(false)
}

/// Handle the signals shisad sends to this agent. Runs for the life of the
/// process: the connection to the bus stays up across shisad restarts.
fn signal_loop(
    conn: &Connection,
    handle: &Handle<TrayState>,
    gate: &Arc<Mutex<Option<window::Gate>>>,
    tracker: &Arc<Mutex<window::Tracker>>,
) {
    let rule = match MatchRule::builder()
        .msg_type(Type::Signal)
        .sender(BUS_NAME)
        .and_then(|rule| rule.interface(INTERFACE))
        .map(|rule| rule.build())
    {
        Ok(rule) => rule,
        Err(e) => {
            error!("match rule: {e}");
            return;
        }
    };
    let mut signals = match MessageIterator::for_match_rule(rule, conn, None) {
        Ok(signals) => signals,
        Err(e) => {
            error!("signals: {e}");
            return;
        }
    };
    for message in &mut signals {
        let message = match message {
            Ok(message) => message,
            Err(e) => {
                // A lagging receiver or a closed stream must not end the
                // loop: the tray would keep claiming to be online while no
                // state change ever arrives.
                warn!("signal stream: {e}");
                thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        let Some(member) = message.header().member().map(|m| m.as_str().to_string()) else {
            continue;
        };
        if member == "StateChanged"
            && let Ok(manager) = ManagerProxyBlocking::new(conn)
        {
            refresh(handle, &manager, gate, tracker);
        }
        if member == "SessionUnlocked" {
            // KWin drops the window changes the script makes while the lock
            // screen is still up. The unlock hint arrives a moment before
            // the surface is gone, so run the gate again after a short wait,
            // twice to cover a slower fade.
            info!("screen unlocked, reapplying the window gate");
            let gate = gate.clone();
            let handle = handle.clone();
            let tracker = tracker.clone();
            let conn = conn.clone();
            thread::spawn(move || {
                for delay in [Duration::from_millis(300), Duration::from_millis(1200)] {
                    thread::sleep(delay);
                    if let Ok(manager) = ManagerProxyBlocking::new(&conn) {
                        refresh(&handle, &manager, &gate, &tracker);
                    }
                    if let Some(gate) = gate.lock().unwrap().as_mut() {
                        gate.force();
                    }
                }
            });
        }
    }
    error!("signal stream ended");
}

fn refresh(
    handle: &Handle<TrayState>,
    manager: &ManagerProxyBlocking<'_>,
    gate: &Arc<Mutex<Option<window::Gate>>>,
    tracker: &Arc<Mutex<window::Tracker>>,
) {
    let Ok(apps) = manager.list_apps() else {
        return;
    };
    // The unfocus delays change with the settings, which a `StateChanged`
    // does not cover. Read them on every refresh. A failed call keeps the
    // last good map: a transient bus error is not a reason to drop the clock
    // of every app.
    match manager.relock_seconds() {
        Ok(relock) => {
            tracker.lock().unwrap().relock = relock.into_iter().collect();
        }
        Err(e) => warn!("relock seconds: {e}"),
    }
    // Hide or show the windows of the protected apps to match the window
    // state shisad just reported. Locks and activations land here. The
    // window state is separate from the tickets, so an expiring ticket does
    // not hide a window that is still in use.
    let protected: Vec<String> = apps.iter().map(|(id, _, _, _)| id.clone()).collect();
    // A failed call is not "nothing is allowed". Keep the window layer as it
    // is rather than hiding every protected window on a transient error.
    let mut gate = gate.lock().unwrap();
    match manager.window_states() {
        Ok(allowed) => {
            // A window that is not allowed any more is hidden. A relock clock
            // started before the lock must not survive it, or it would hide
            // the window again shortly after the user unlocks and shows it.
            tracker.lock().unwrap().retain_allowed(&allowed);
            if let Some(gate) = gate.as_mut() {
                gate.sync(&protected, &allowed, false);
            }
        }
        Err(e) => {
            warn!("window states: {e}");
            return;
        }
    };
    drop(gate);
    let _ = handle.update(|tray| tray.protected_count = protected.len());
}

fn action_loop(
    rx: Receiver<Action>,
    gate: &Arc<Mutex<Option<window::Gate>>>,
    handle: &Handle<TrayState>,
    dnd_hinted: &AtomicBool,
) {
    for action in rx {
        match action {
            Action::LockAll => match lock("") {
                Ok(()) => verify_lock(gate, handle, dnd_hinted),
                Err(e) => {
                    let language = i18n::current();
                    let message = match language {
                        Language::ZhCn => format!("全部上锁失败：{e}"),
                        Language::En => format!("Lock all failed: {e}"),
                    };
                    notify_lock(handle, &message, false, NOTIFY_PROBLEM_TIMEOUT_MS);
                }
            },
            Action::VerifyLock => verify_lock(gate, handle, dnd_hinted),
            Action::Unlock(app) => unlock(&app, gate),
            Action::RestoreIfTicketed(app) => restore_if_ticketed(&app, gate),
            Action::HiddenWindows(version, counts) => {
                let _ = handle.update(|tray| {
                    if version > tray.hidden_windows.0 {
                        tray.hidden_windows = (version, counts);
                    }
                });
            }
            Action::OpenSettings => open_settings(gate, false),
            Action::OpenRestore => open_settings(gate, true),
        }
    }
}

fn lock(app: &str) -> zbus::Result<()> {
    let conn = Connection::system()?;
    ManagerProxyBlocking::new(&conn)?.lock(app)
}

fn verify_lock(
    gate: &Mutex<Option<window::Gate>>,
    handle: &Handle<TrayState>,
    dnd_hinted: &AtomicBool,
) {
    let report = (|| -> Option<WindowReport> {
        let mut gate = gate.lock().unwrap();
        let gate = gate.as_mut()?;
        let conn = Connection::system().ok()?;
        let manager = ManagerProxyBlocking::new(&conn).ok()?;
        let protected = manager
            .list_apps()
            .ok()?
            .into_iter()
            .map(|row| row.0)
            .collect::<Vec<_>>();
        let allowed = manager.window_states().ok()?;
        if !allowed.is_empty() {
            return None;
        }
        let report = gate.sync(&protected, &allowed, true)?;
        // An unlock can complete while KWin is compiling the script.
        if !manager.window_states().ok()?.is_empty() {
            return None;
        }
        Some(report)
    })();
    let language = i18n::current();
    let names = report
        .as_ref()
        .map(|report| missed_names(&report.missed, language))
        .unwrap_or_default();
    let hint =
        report.as_ref().is_some_and(|report| report.hidden > 0) && should_hint_dnd(dnd_hinted);
    let message = lock_summary(report.as_ref(), language, hint, &names);
    let concealed = report.as_ref().is_some_and(|report| report.hidden > 0);
    let timeout = match &report {
        Some(report) if report.missed.is_empty() && report.matched == report.hidden => {
            NOTIFY_TIMEOUT_MS
        }
        _ => NOTIFY_PROBLEM_TIMEOUT_MS,
    };
    notify_lock(handle, &message, concealed, timeout);
}

/// The display names of the apps that could not be concealed. A malformed or
/// unknown desktop file falls back to the app id, which is still useful.
fn missed_names(missed: &[(String, u32)], language: Language) -> String {
    if missed.is_empty() {
        return String::new();
    }
    let env = shisa_core::desktop::Env::current();
    let mut names: Vec<String> = missed
        .iter()
        .map(|(app, _)| shisa_core::desktop::display_name(app, &env))
        .collect();
    names.sort();
    names.dedup();
    let shown = names.len().min(3);
    let mut text = names[..shown].join(match language {
        Language::ZhCn => "、",
        Language::En => ", ",
    });
    if names.len() > shown {
        let rest = names.len() - shown;
        text.push_str(&match language {
            Language::ZhCn => format!(" 等 {rest} 个应用"),
            Language::En => format!(" and {rest} more"),
        });
    }
    text
}

fn lock_summary(
    report: Option<&WindowReport>,
    language: Language,
    hint: bool,
    names: &str,
) -> String {
    let mut message = match (report, language) {
        (Some(report), Language::ZhCn) => {
            if report.matched == 0 {
                "启动已上锁，当前桌面未匹配到受保护窗口".into()
            } else if !report.missed.is_empty() && !names.is_empty() {
                format!(
                    "启动已上锁，已收起 {}/{} 个受保护窗口，未确认：{names}",
                    report.hidden, report.matched
                )
            } else if report.missed.is_empty() && report.matched == report.hidden {
                format!("启动已上锁，已收起 {} 个受保护窗口", report.hidden)
            } else {
                "已执行启动上锁，窗口状态未确认".into()
            }
        }
        (Some(report), Language::En) => {
            if report.matched == 0 {
                "Locked, but no protected window was found on this desktop".into()
            } else if !report.missed.is_empty() && !names.is_empty() {
                format!(
                    "Locked and hid {} of {} protected windows. Not confirmed: {names}",
                    report.hidden, report.matched
                )
            } else if report.missed.is_empty() && report.matched == report.hidden {
                match report.hidden {
                    1 => "Locked and hid 1 protected window".into(),
                    hidden => format!("Locked and hid {hidden} protected windows"),
                }
            } else {
                "Lock applied, but the window state was not confirmed".into()
            }
        }
        (None, Language::ZhCn) => "已执行启动上锁，窗口状态未确认".into(),
        (None, Language::En) => "Lock applied, but the window state was not confirmed".into(),
    };
    // Only a lock that actually concealed something raises the notification
    // concern. The hint tells the owner what the lock does not cover, so it
    // belongs next to the result rather than only in the documentation.
    if hint && report.is_some_and(|report| report.hidden > 0) {
        message.push_str(match language {
            Language::ZhCn => {
                "\n建议开启勿扰：受保护应用的通知仍会弹出，通知历史也能打开。"
            }
            Language::En => {
                "\nConsider Do Not Disturb: protected apps' notifications still appear and their history stays accessible."
            }
        });
    }
    message
}

fn notify_lock(handle: &Handle<TrayState>, message: &str, concealed: bool, timeout_ms: i32) {
    info!("{message}");
    let _ = handle.update(|tray| tray.last_lock = Some(message.to_string()));
    let icon = if concealed {
        LOCKED_ICON
    } else {
        "shisa-tray-symbolic"
    };
    notify(message, timeout_ms, icon);
}

/// Send one lock result. The notification is a confirmation with no action,
/// and every lock gets its own popup so a repeated lock is confirmed again
/// after the previous result has expired.
fn notify(message: &str, timeout_ms: i32, icon: &str) {
    let result = (|| -> zbus::Result<u32> {
        let conn = Connection::session()?;
        let proxy = Proxy::new(
            &conn,
            NOTIFY_SERVICE,
            "/org/freedesktop/Notifications",
            NOTIFY_SERVICE,
        )?;
        let hints = HashMap::<String, zbus::zvariant::Value>::new();
        let id = proxy.call(
            "Notify",
            &(
                "Shisa",
                0u32,
                icon,
                "Shisa",
                message,
                Vec::<String>::new(),
                hints,
                timeout_ms,
            ),
        )?;
        Ok(id)
    })();
    if let Err(e) = result {
        warn!("lock notification: {e}");
    }
}

/// Plasma's effective Do Not Disturb state, read from the notification
/// service. `None` means the property is absent, the service is not running,
/// or the session bus is unavailable.
fn do_not_disturb() -> Option<bool> {
    // A hung plasmashell can still own the name, so the property read gets a
    // short timeout instead of the connection's default 25 seconds. The lock
    // notification must not wait on a desktop that stopped answering.
    let conn = zbus::blocking::connection::Builder::session()
        .ok()?
        .method_timeout(Duration::from_millis(750))
        .build()
        .ok()?;
    // Reading the property can activate the notification service and block
    // when plasmashell is not running. The name check keeps a lock from
    // waiting on that.
    if !name_has_owner(&conn, NOTIFY_SERVICE) {
        return None;
    }
    let reply = conn
        .call_method(
            Some(NOTIFY_SERVICE),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(NOTIFY_SERVICE, "Inhibited"),
        )
        .ok()?;
    let value = reply
        .body()
        .deserialize::<zbus::zvariant::OwnedValue>()
        .ok()?;
    bool::try_from(value).ok()
}

/// Whether to add the Do Not Disturb reminder. A known false is worth saying
/// on every lock. An unknown state is Plasma-specific, so remind once per
/// session instead of on every lock.
fn should_hint_dnd(shown: &AtomicBool) -> bool {
    match do_not_disturb() {
        Some(true) => false,
        Some(false) => true,
        None => !shown.swap(true, Ordering::Relaxed),
    }
}

/// Show one hidden app again from the tray. A valid ticket from the last
/// authentication is enough, so the desktop's polkit box only appears once
/// the grace period has run out. The action thread is separate, so the menu
/// stays responsive while the box waits.
fn unlock(app: &str, gate: &Arc<Mutex<Option<window::Gate>>>) {
    let result = (|| -> zbus::Result<bool> {
        let conn = Connection::system()?;
        let manager = ManagerProxyBlocking::new(&conn)?;
        if manager.window_states()?.iter().any(|id| id == app) {
            return Ok(false);
        }
        // An older shisad does not serve Unhide. Fall back to authentication
        // instead of leaving the window hidden.
        match manager.unhide(app) {
            Ok(true) => {
                info!(app, "window permission restored from its current ticket");
                return Ok(false);
            }
            Ok(false) => {}
            Err(e) => debug!("unhide {app}: {e}"),
        }
        manager.unlock(app)?;
        Ok(true)
    })();
    match result {
        Ok(authenticated) => {
            if authenticated {
                info!(app, "unlock authenticated");
            }
            restore_window(app, gate);
        }
        Err(e) => warn!("unlock: {e}"),
    }
}

/// Restore a window the user asked for through the app's own tray icon.
/// The KWin script hid it again, so it stays hidden until this confirms a
/// valid ticket. Without one the window stays hidden and no prompt starts:
/// the restore panel remains the way to authenticate.
fn restore_if_ticketed(app: &str, gate: &Arc<Mutex<Option<window::Gate>>>) {
    let shown = (|| -> zbus::Result<bool> {
        let conn = Connection::system()?;
        let manager = ManagerProxyBlocking::new(&conn)?;
        manager.unhide(app)
    })();
    match shown {
        Ok(true) => restore_window(app, gate),
        Ok(false) => debug!(app, "no ticket, the window stays hidden"),
        Err(e) => warn!("restore {app}: {e}"),
    }
}

fn restore_window(app: &str, gate: &Mutex<Option<window::Gate>>) {
    let confirmed = (|| -> Option<bool> {
        let mut gate = gate.lock().unwrap();
        let gate = gate.as_mut()?;
        let conn = Connection::system().ok()?;
        let manager = ManagerProxyBlocking::new(&conn).ok()?;
        let protected = manager
            .list_apps()
            .ok()?
            .into_iter()
            .map(|row| row.0)
            .collect::<Vec<_>>();
        let allowed = manager.window_states().ok()?;
        Some(allowed.iter().any(|id| id == app) && gate.restore(&protected, &allowed, app))
    })();
    if confirmed != Some(true) {
        notify(
            text(
                i18n::current(),
                "窗口恢复未确认",
                "Window restore was not confirmed",
            ),
            NOTIFY_PROBLEM_TIMEOUT_MS,
            "shisa-tray-symbolic",
        );
    }
}

/// Hide an app whose window sat unfocused longer than its configured delay.
/// This drops the window permission but keeps the app's ticket, so restoring
/// the window inside the grace period does not ask for authentication again.
/// The StateChanged that follows makes the resident script hide the window.
fn relock_loop(conn: &Connection, tracker: &Arc<Mutex<window::Tracker>>) -> ! {
    loop {
        // Short enough that the immediate preset feels immediate, cheap
        // enough to leave running.
        thread::sleep(Duration::from_millis(250));
        let due = tracker.lock().unwrap().take_due(Instant::now());
        if due.is_empty() {
            continue;
        }
        let Ok(manager) = ManagerProxyBlocking::new(conn) else {
            continue;
        };
        for app in due {
            if !tracker.lock().unwrap().may_relock(&app) {
                continue;
            }
            info!(%app, "window sat unfocused, hiding it");
            if let Err(e) = manager.hide(&app) {
                // A new agent can meet an older shisad that has no Hide. A
                // lock is the old behavior and still conceals the window,
                // even though it also drops the ticket.
                debug!("relock {app}: {e}; falling back to a lock");
                if let Err(e) = manager.lock(&app) {
                    debug!("relock {app}: lock fallback failed: {e}");
                }
            }
        }
    }
}

/// The display variables the systemd user manager knows about and this
/// process lacks.
///
/// A user unit can start before the compositor exports `WAYLAND_DISPLAY` or
/// `DISPLAY` to the manager, so the agent's own environment may not have
/// them. The settings window is GTK and aborts during startup without one
/// (`gtk::rt::init` fails), so the missing values are copied from the manager
/// onto the child.
fn display_env() -> Vec<(String, String)> {
    select_display_vars(manager_environment(), |key| env::var_os(key).is_some())
}

/// The whole environment the session's systemd user manager exports.
fn manager_environment() -> Vec<String> {
    let Ok(connection) = Connection::session() else {
        return Vec::new();
    };
    let Ok(manager) = Proxy::new(
        &connection,
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1",
        "org.freedesktop.systemd1.Manager",
    ) else {
        return Vec::new();
    };
    manager
        .get_property::<Vec<String>>("Environment")
        .unwrap_or_default()
}

/// Keep the manager's display variables that `present` does not already
/// report.
fn select_display_vars(
    variables: Vec<String>,
    present: impl Fn(&str) -> bool,
) -> Vec<(String, String)> {
    const KEYS: [&str; 3] = ["WAYLAND_DISPLAY", "DISPLAY", "XAUTHORITY"];
    variables
        .into_iter()
        .filter_map(|entry| {
            entry
                .split_once('=')
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
        })
        .filter(|(key, _)| KEYS.contains(&key.as_str()) && !present(key))
        .collect()
}

fn open_settings(gate: &Arc<Mutex<Option<window::Gate>>>, restore: bool) {
    let bin = env::var_os("SHISA_SETTINGS_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "/usr/bin/shisa-settings".into());
    let missing = display_env();
    if !missing.is_empty() {
        debug!(
            keys = missing.len(),
            "settings: display env from the user manager"
        );
    }
    // The agent's user unit is a cgroup. Stopping or restarting it would kill
    // every process in that group, including an open settings window. A
    // transient scope gives the window its own lifetime, so the user can
    // keep reading the health line while the agent is down.
    let mut settings = Command::new("/usr/bin/systemd-run");
    settings
        .args(["--user", "--scope", "--collect", "--quiet", "--"])
        .arg(bin);
    if restore {
        settings.arg("--restore-windows");
    }
    settings.envs(missing);
    match settings.spawn() {
        // The agent outlives every settings process, so each launch that
        // nobody waits for stays a zombie. A second instance exits at once
        // under Tauri's single-instance plugin, so the count grows quickly.
        Ok(child) => {
            info!("settings opened");
            // The tray brings an already open settings window forward. Wayland
            // ignores the toolkit's focus request without an activation token,
            // so ask KWin for it, the same way the window layer restores an
            // app. Two tries cover a window that is still being created.
            if !restore {
                let gate = gate.clone();
                thread::spawn(move || {
                    for delay in [Duration::from_millis(300), Duration::from_millis(900)] {
                        thread::sleep(delay);
                        if let Some(gate) = gate.lock().unwrap().as_mut() {
                            gate.activate(BUS_NAME);
                        }
                    }
                });
            }
            thread::spawn(move || {
                let mut child = child;
                let _ = child.wait();
            });
        }
        Err(e) => error!("settings: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_menu_shows_three_actions_and_counts_hidden_windows() {
        let (actions, requests) = mpsc::channel();
        let mut tray = TrayState {
            hidden_windows: ((1, 1), HashMap::from([("org.kde.kate".into(), 2)])),
            protected_count: 2,
            last_lock: Some("已收起 2 个窗口".into()),
            offline: false,
            language: Language::ZhCn,
            actions,
        };
        let menu = tray.menu();
        let [
            MenuItem::Standard(lock_all),
            MenuItem::Separator,
            MenuItem::Standard(settings),
        ] = menu.as_slice()
        else {
            panic!("expected lock and settings with a separator");
        };

        assert!(tray.tool_tip().description.contains("已隐藏 2 个窗口"));
        (lock_all.activate)(&mut tray);
        assert!(matches!(requests.try_recv(), Ok(Action::LockAll)));
        (settings.activate)(&mut tray);
        assert!(matches!(requests.try_recv(), Ok(Action::OpenSettings)));
    }

    #[test]
    fn the_tray_icon_shows_a_lock_while_windows_are_hidden() {
        let (actions, _requests) = mpsc::channel();
        let mut tray = TrayState {
            hidden_windows: Default::default(),
            protected_count: 1,
            last_lock: None,
            offline: false,
            language: Language::ZhCn,
            actions,
        };
        assert_eq!(tray.icon_name(), "shisa-tray-symbolic");
        tray.hidden_windows = ((1, 1), HashMap::from([("org.kde.kate".into(), 2)]));
        assert_eq!(tray.icon_name(), LOCKED_ICON);
        tray.offline = true;
        assert_eq!(tray.icon_name(), OFFLINE_ICON);
    }

    fn report(matched: u32, hidden: u32, missed: &[(&str, u32)]) -> WindowReport {
        WindowReport {
            matched,
            hidden,
            missed: missed
                .iter()
                .map(|(app, count)| ((*app).to_string(), *count))
                .collect(),
        }
    }

    #[test]
    fn lock_feedback_requires_all_matched_windows_to_be_concealed() {
        let none = report(0, 0, &[]);
        let full = report(2, 2, &[]);
        let partial = report(3, 2, &[("org.kde.kate", 1)]);
        assert!(lock_summary(Some(&none), Language::ZhCn, false, "").contains("未匹配"));
        assert!(lock_summary(Some(&full), Language::ZhCn, false, "").contains("已收起 2"));
        assert!(
            lock_summary(Some(&partial), Language::ZhCn, false, "app.one")
                .contains("未确认：app.one")
        );
        assert_eq!(
            lock_summary(Some(&report(2, 1, &[])), Language::ZhCn, false, ""),
            lock_summary(None, Language::ZhCn, false, "")
        );
        assert!(lock_summary(None, Language::ZhCn, false, "").contains("未确认"));

        assert!(lock_summary(Some(&none), Language::En, false, "").contains("no protected window"));
        assert!(lock_summary(Some(&full), Language::En, false, "").contains("hid 2"));
        assert!(
            lock_summary(Some(&partial), Language::En, false, "app.one")
                .contains("Not confirmed: app.one")
        );
        assert_eq!(
            lock_summary(Some(&report(2, 1, &[])), Language::En, false, ""),
            lock_summary(None, Language::En, false, "")
        );
        assert!(lock_summary(None, Language::En, false, "").contains("not confirmed"));
    }

    #[test]
    fn the_dnd_hint_follows_a_lock_that_hid_something() {
        let full = report(2, 2, &[]);
        assert!(lock_summary(Some(&full), Language::ZhCn, true, "").contains("勿扰"));
        assert!(!lock_summary(Some(&full), Language::ZhCn, false, "").contains("勿扰"));
        // Nothing was hidden, so notifications from hidden apps are not the
        // concern this hint addresses.
        let none = report(0, 0, &[]);
        assert!(!lock_summary(Some(&none), Language::ZhCn, true, "").contains("勿扰"));
    }

    #[test]
    fn only_missing_display_vars_are_copied() {
        let variables = vec![
            "WAYLAND_DISPLAY=wayland-1".to_string(),
            "DISPLAY=:0".to_string(),
            "XAUTHORITY=/run/user/1000/xauth".to_string(),
            "LANG=zh_CN.UTF-8".to_string(),
        ];
        let picked = select_display_vars(variables, |key| key == "WAYLAND_DISPLAY");
        assert_eq!(
            picked,
            vec![
                ("DISPLAY".to_string(), ":0".to_string()),
                ("XAUTHORITY".to_string(), "/run/user/1000/xauth".to_string()),
            ]
        );
    }

    // Meaningful only in a desktop session: it checks that the user manager's
    // exported environment reaches the settings process. Without a user
    // manager (CI containers have none) there is nothing to observe, and with
    // an exported WAYLAND_DISPLAY there is nothing to copy. Run it with:
    //   env -u WAYLAND_DISPLAY -u DISPLAY cargo test -p shisa-agent manager
    #[test]
    fn the_manager_supplies_wayland_display_when_the_agent_lacks_it() {
        let variables = manager_environment();
        let manager_exports_it = variables
            .iter()
            .any(|entry| entry.starts_with("WAYLAND_DISPLAY="));
        if env::var_os("WAYLAND_DISPLAY").is_some() || !manager_exports_it {
            return;
        }
        let picked = select_display_vars(variables, |key| env::var_os(key).is_some());
        assert!(
            picked.iter().any(|(key, _)| key == "WAYLAND_DISPLAY"),
            "the user manager did not report WAYLAND_DISPLAY: {picked:?}"
        );
    }

    #[test]
    fn a_complete_environment_gets_nothing() {
        let variables = vec!["WAYLAND_DISPLAY=wayland-1".to_string()];
        assert!(select_display_vars(variables, |_| true).is_empty());
    }
}
