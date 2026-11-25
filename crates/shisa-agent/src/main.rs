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
use shisa_core::dbus::{
    AppRow, BUS_NAME, INTERFACE, ManagerProxyBlocking, WINDOW_BUS_NAME, WINDOW_PATH,
};
use shisa_core::i18n::{self, Language};
use std::collections::HashMap;
use std::env;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};
use zbus::MatchRule;
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::message::Type;

mod window;

use window::{NoReport, WindowReport};

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
/// Used while at least one concealed window would ask for the app-lock
/// password to restore it.
const LOCKED_ICON: &str = "shisa-tray-locked-symbolic";
/// Used while at least one concealed app still has a valid launch ticket, so
/// restoring it within the grace period asks no password.
const RESTORABLE_ICON: &str = "shisa-tray-restorable-symbolic";
const NOTIFY_SERVICE: &str = "org.freedesktop.Notifications";
/// How long the desktop keeps a lock result notification on screen. A
/// result that still needs attention stays longer.
const NOTIFY_TIMEOUT_MS: i32 = 5_000;
const NOTIFY_PROBLEM_TIMEOUT_MS: i32 = 8_000;
/// A focus-loss hide is silent by design, so the first one explains where
/// the window went. Later hides stay quiet: the user opted into the feature
/// and the tray icon already shows the locked state.
static RELOCK_HINTED: AtomicBool = AtomicBool::new(false);
/// Bound the retries for a notification service that is not delivering the
/// hint. Three attempts cover a startup race without spawning a thread for
/// every hide while DND or a broken service suppresses the popup.
static RELOCK_HINT_ATTEMPTS: AtomicU8 = AtomicU8::new(0);
const RELOCK_HINT_MAX_ATTEMPTS: u8 = 3;

struct TrayState {
    protected_count: usize,
    hidden_windows: ((u64, u64), HashMap<String, u32>),
    /// Concealed apps that still have a valid launch ticket, so restoring
    /// them would not ask for the app-lock password. `None` means no daemon
    /// read has confirmed it, and the tray keeps the locked state.
    hidden_ticketed: Option<usize>,
    last_lock: Option<String>,
    offline: bool,
    /// Ticket grants expected to bring a concealed window back, with the
    /// deadline for its hidden report. The daemon allows the window at the
    /// same time as the ticket, but KWin reports it hidden until it applies
    /// the change, so ticket reads keep the previous ring color while one is
    /// pending instead of flashing amber before the restored green.
    ticket_grants: HashMap<String, Instant>,
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

/// Concealed apps that still have a valid launch ticket, so the restore panel
/// can bring them back without a prompt.
fn ticketed_hidden(apps: &[AppRow], hidden: &HashMap<String, u32>) -> usize {
    apps.iter()
        .filter(|(id, unlocked, _, _)| *unlocked && hidden.contains_key(id))
        .count()
}

/// What one ticket poll should show. `Skip` keeps the last value because
/// shisad is offline, `Confirmed` replaces it with the number of hidden apps
/// that can still restore without a password, and `Unconfirmed` is a read
/// that did not come back.
#[derive(Debug, PartialEq, Eq)]
enum TicketPoll {
    Skip,
    Confirmed(usize),
    Unconfirmed,
}

/// Decide the tray's ticket state for one poll. Kept apart from the loop so
/// the decisions can be tested without a live agent.
fn ticket_poll(
    hidden: &HashMap<String, u32>,
    offline: bool,
    read: Option<zbus::Result<Vec<AppRow>>>,
) -> TicketPoll {
    if offline {
        return TicketPoll::Skip;
    }
    if hidden.is_empty() {
        return TicketPoll::Confirmed(0);
    }
    match read {
        Some(Ok(apps)) => TicketPoll::Confirmed(ticketed_hidden(&apps, hidden)),
        Some(Err(_)) | None => TicketPoll::Unconfirmed,
    }
}

/// Reads the daemon's ticket state for the apps a report says are hidden.
/// The action thread uses it to publish a new hidden set together with its
/// ticket count, so a hide that is still ticketed goes straight to amber
/// instead of showing the blue unconfirmed fallback until the next poll.
#[derive(Default)]
struct TicketReader(Option<Connection>);

impl TicketReader {
    /// How many hidden apps still have a valid launch ticket. `None` means
    /// the daemon could not be asked, which keeps the tray on the locked
    /// fallback, the same decision `ticket_poll` makes.
    fn read(&mut self, hidden: &HashMap<String, u32>) -> Option<usize> {
        if hidden.is_empty() {
            return Some(0);
        }
        if self.0.is_none() {
            self.0 = Connection::system().ok();
        }
        let apps = self
            .0
            .as_ref()
            .and_then(|conn| ManagerProxyBlocking::new(conn).ok())
            .and_then(|manager| manager.list_apps().ok());
        if apps.is_none() {
            // Reconnect on the next read: the bus may have restarted.
            self.0 = None;
        }
        apps.map(|apps| ticketed_hidden(&apps, hidden))
    }
}

/// Publish a script's hidden-window report together with the ticket count
/// read after it arrived. Both fields change in one update so the ring goes
/// from green straight to amber when the hidden app still has a ticket,
/// instead of showing the blue unconfirmed fallback first. `None` for the
/// count means the daemon could not be asked and keeps that fallback.
fn apply_hidden_windows(
    tray: &mut TrayState,
    version: (u64, u64),
    counts: HashMap<String, u32>,
    ticketed: Option<usize>,
) -> bool {
    if version <= tray.hidden_windows.0 {
        return false;
    }
    // A report that no longer lists the granted app means its window came
    // back (or closed), so the grant's hold has served its purpose.
    let now = Instant::now();
    tray.ticket_grants
        .retain(|app, until| now < *until && counts.contains_key(app));
    tray.hidden_windows = (version, counts);
    publish_ticket_state(tray, ticketed);
    true
}

/// How long a ticket grant may take to show its window. One revision load can
/// wait up to 1.2 seconds for the script acknowledgement, and the script polls
/// every 250 milliseconds. The hold only suppresses the ticket color, and a
/// hidden report that brings the window back ends it sooner.
const TICKET_GRANT_HOLD: Duration = Duration::from_secs(3);

/// Remember a ticket grant that should bring its window back. Only an app the
/// script still reports hidden needs the hold: a grant for a window that is
/// already visible changes no concealed state.
fn hold_ticket_grant(tray: &mut TrayState, app: &str, now: Instant) {
    if tray.hidden_windows.1.contains_key(app) {
        tray.ticket_grants
            .insert(app.to_string(), now + TICKET_GRANT_HOLD);
    }
}

/// Publish a ticket state read from the daemon. While a grant is expected to
/// bring a concealed window back, the ring keeps its previous color: the
/// ticket exists before KWin reports the window shown, so counting it now
/// would flash amber. A failed read holds the same way, and shisad going away
/// still turns the ring red on its own.
fn publish_ticket_state(tray: &mut TrayState, ticketed: Option<usize>) {
    let now = Instant::now();
    tray.ticket_grants.retain(|_, until| now < *until);
    if tray.ticket_grants.is_empty() {
        tray.hidden_ticketed = ticketed;
    }
}

/// Apply one background ticket poll. Runs under the ticket lock, so the
/// count was read against the same hidden set that is still current.
fn apply_ticket_poll(tray: &mut TrayState, poll: TicketPoll) {
    match poll {
        TicketPoll::Skip => {}
        TicketPoll::Confirmed(ticketed) => publish_ticket_state(tray, Some(ticketed)),
        TicketPoll::Unconfirmed => publish_ticket_state(tray, None),
    }
}

/// Keep the hidden apps' ticket state fresh. A ticket expiring is a lazy
/// check with no signal, so the tray would keep showing the password-free
/// dot after the grace period ended. Two seconds matches the language
/// watcher and the restore panel's poll, and the bus call only runs while a
/// window is hidden and shisad is known to be online.
fn watch_ticket_state(handle: Handle<TrayState>, ticket_sync: &Mutex<()>) {
    let mut connection: Option<Connection> = None;
    loop {
        thread::sleep(Duration::from_secs(2));
        // The read and the update happen under the same lock the refresh and
        // the hidden report use, so a count taken before a lock or unlock
        // cannot land after a fresher one.
        let _sync = ticket_sync.lock().unwrap();
        let Some((hidden, offline)) =
            handle.update(|tray| (tray.hidden_windows.1.clone(), tray.offline))
        else {
            return;
        };
        let read = if offline || hidden.is_empty() {
            None
        } else {
            if connection.is_none() {
                connection = Connection::system().ok();
            }
            connection
                .as_ref()
                .map(|conn| ManagerProxyBlocking::new(conn).and_then(|manager| manager.list_apps()))
        };
        match ticket_poll(&hidden, offline, read) {
            TicketPoll::Skip => {}
            TicketPoll::Confirmed(ticketed) => {
                let _ =
                    handle.update(|tray| apply_ticket_poll(tray, TicketPoll::Confirmed(ticketed)));
            }
            TicketPoll::Unconfirmed => {
                // A failed read is not proof that a ticket is still valid.
                // Drop to the locked state and reconnect on the next tick.
                connection = None;
                let _ = handle.update(|tray| apply_ticket_poll(tray, TicketPoll::Unconfirmed));
            }
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
        } else if self.hidden_count() == 0 {
            "shisa-tray-symbolic"
        } else if self.hidden_ticketed.unwrap_or(0) > 0 {
            RESTORABLE_ICON
        } else {
            LOCKED_ICON
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
            match self.hidden_ticketed {
                Some(ticketed) if ticketed > 0 => {
                    description.push_str(&match (ticketed, self.language) {
                        (1, Language::ZhCn) => "\n其中 1 个应用可免密还原".to_string(),
                        (ticketed, Language::ZhCn) => format!("\n其中 {ticketed} 个应用可免密还原"),
                        (1, Language::En) => {
                            "\n1 app can restore without the app-lock password".to_string()
                        }
                        (ticketed, Language::En) => {
                            format!("\n{ticketed} apps can restore without the app-lock password")
                        }
                    });
                }
                Some(_) => {
                    description.push_str(text(
                        self.language,
                        "\n恢复需要应用锁密码",
                        "\nRestoring asks for the app-lock password",
                    ));
                }
                // `None` means no daemon read has confirmed the ticket state.
                // Saying the password is required would turn an unknown state
                // into a claim the tray cannot support.
                None => {
                    description.push_str(text(
                        self.language,
                        "\n还原凭证状态未确认",
                        "\nThe restore ticket state is unconfirmed",
                    ));
                }
            }
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
        // A disabled row that only says "Lock protected apps" leaves the
        // reason to the tooltip. The menu is read on its own, so the reason
        // belongs in the label.
        let lock_label = if self.offline {
            text(
                self.language,
                "锁定受保护应用（离线）",
                "Lock protected apps (offline)",
            )
        } else if self.protected_count == 0 {
            text(self.language, "没有受保护的应用", "No protected apps")
        } else {
            text(self.language, "锁定受保护应用", "Lock protected apps")
        };
        vec![
            StandardItem {
                label: text(self.language, "恢复窗口…", "Restore windows…").into(),
                activate: send(Action::OpenRestore),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: lock_label.into(),
                enabled: !self.offline && self.protected_count > 0,
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
    // Serializes every read of the daemon's ticket state with the tray update
    // that publishes it. Without it, a read taken before a lock or unlock
    // could land after a fresher read and leave the ring on the wrong color
    // until the next poll (docs/subsystems/window-management.md).
    let ticket_sync: Arc<Mutex<()>> = Arc::default();
    let tray = TrayState {
        protected_count: 0,
        hidden_windows: Default::default(),
        hidden_ticketed: None,
        last_lock: None,
        offline: true,
        ticket_grants: Default::default(),
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
        let handle = handle.clone();
        let ticket_sync = ticket_sync.clone();
        thread::Builder::new()
            .name("tickets".into())
            .spawn(move || watch_ticket_state(handle, &ticket_sync))
            .expect("ticket thread");
    }
    {
        let gate = gate.clone();
        let handle = handle.clone();
        let dnd_hinted = Arc::new(AtomicBool::new(false));
        let ticket_sync = ticket_sync.clone();
        thread::Builder::new()
            .name("actions".into())
            .spawn(move || action_loop(actions_rx, &gate, &handle, &dnd_hinted, &ticket_sync))
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
        let ticket_sync = ticket_sync.clone();
        thread::Builder::new()
            .name("signals".into())
            .spawn(move || signal_loop(&conn, &handle, &gate, &tracker, &ticket_sync))
            .expect("signals thread");
    }
    supervise(&conn, &handle, &gate, &tracker, &ticket_sync);
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
    ticket_sync: &Mutex<()>,
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
                        refresh(handle, &manager, gate, tracker, ticket_sync);
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
    ticket_sync: &Arc<Mutex<()>>,
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
            // A grant that brings a concealed window back shows it within a
            // script poll and at most one revision load. Hold the ring's
            // previous color until the hidden report arrives, so the grant
            // does not flash amber between the locked blue and the restored
            // green.
            if let Ok((app, true)) = message.body().deserialize::<(String, bool)>() {
                let _ = handle.update(|tray| hold_ticket_grant(tray, &app, Instant::now()));
            }
            refresh(handle, &manager, gate, tracker, ticket_sync);
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
            let ticket_sync = ticket_sync.clone();
            thread::spawn(move || {
                for delay in [Duration::from_millis(300), Duration::from_millis(1200)] {
                    thread::sleep(delay);
                    if let Ok(manager) = ManagerProxyBlocking::new(&conn) {
                        refresh(&handle, &manager, &gate, &tracker, &ticket_sync);
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
    ticket_sync: &Mutex<()>,
) {
    // Publish the protected and ticket counts under the ticket lock, from one
    // daemon read, before the window-state sync. A failed WindowStates call
    // must not leave the tray's lock action disabled with a stale zero. The
    // lock keeps a read taken before a lock or unlock from landing after a
    // fresher read.
    let apps = {
        let _sync = ticket_sync.lock().unwrap();
        let Ok(apps) = manager.list_apps() else {
            return;
        };
        let _ = handle.update(|tray| {
            let ticketed = ticketed_hidden(&apps, &tray.hidden_windows.1);
            tray.protected_count = apps.len();
            publish_ticket_state(tray, Some(ticketed));
        });
        apps
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
                let _ = gate.sync(&protected, &allowed, false);
            }
        }
        Err(e) => {
            warn!("window states: {e}");
            return;
        }
    };
    drop(gate);
}

fn action_loop(
    rx: Receiver<Action>,
    gate: &Arc<Mutex<Option<window::Gate>>>,
    handle: &Handle<TrayState>,
    dnd_hinted: &AtomicBool,
    ticket_sync: &Mutex<()>,
) {
    let mut tickets = TicketReader::default();
    for action in rx {
        match action {
            Action::LockAll => match lock("") {
                Ok(()) => verify_lock(gate, handle, dnd_hinted),
                Err(e) => {
                    let language = i18n::current();
                    let message = match language {
                        Language::ZhCn => format!("锁定受保护应用失败：{e}"),
                        Language::En => format!("Locking protected apps failed: {e}"),
                    };
                    notify_lock(handle, &message, None, false, NOTIFY_PROBLEM_TIMEOUT_MS);
                }
            },
            Action::VerifyLock => verify_lock(gate, handle, dnd_hinted),
            Action::Unlock(app) => unlock(&app, gate),
            Action::RestoreIfTicketed(app) => restore_if_ticketed(&app, gate),
            Action::HiddenWindows(version, counts) => {
                // Hold the ticket lock across the read and the update so a
                // concurrent refresh cannot interleave a count taken from
                // older daemon state. The read stays outside the tray update
                // because that closure holds the tray's state lock.
                let _sync = ticket_sync.lock().unwrap();
                let fresh = handle
                    .update(|tray| version > tray.hidden_windows.0)
                    .unwrap_or(false);
                if !fresh {
                    continue;
                }
                let ticketed = tickets.read(&counts);
                let _ = handle.update(|tray| apply_hidden_windows(tray, version, counts, ticketed));
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
    // `None` from the closure means the check could not run: the system bus
    // or the daemon failed, or an unlock raced the check. Only a completed
    // sync can tell a desktop without KWin from a KWin that did not answer.
    let check = (|| -> Option<Result<WindowReport, NoReport>> {
        let mut gate = gate.lock().unwrap();
        // No gate means the agent has no window layer at all, for example
        // because the session has no runtime directory. Say so instead of
        // reporting a state that was never checked.
        let Some(gate) = gate.as_mut() else {
            return Some(Err(NoReport::NoWindowLayer));
        };
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
        let report = gate.sync(&protected, &allowed, true);
        // An unlock can complete while KWin is compiling the script.
        if !manager.window_states().ok()?.is_empty() {
            return None;
        }
        Some(report)
    })()
    .unwrap_or(Err(NoReport::Unconfirmed));
    let language = i18n::current();
    let names = match &check {
        Ok(report) => missed_names(&report.missed, language),
        Err(_) => String::new(),
    };
    let concealed = matches!(&check, Ok(report) if report.hidden > 0);
    let hint = concealed && should_hint_dnd(dnd_hinted);
    let text = lock_summary(&check, language, hint, &names);
    let timeout = match &check {
        Ok(report) if report.missed.is_empty() && report.matched == report.hidden => {
            NOTIFY_TIMEOUT_MS
        }
        _ => NOTIFY_PROBLEM_TIMEOUT_MS,
    };
    notify_lock(handle, &text.summary, text.hint, concealed, timeout);
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

/// One lock result. `hint` is advice for the notification popup only: the
/// tray tooltip keeps `summary`, because it stays on screen long after the
/// popup that the hint explains has expired.
struct LockSummary {
    summary: String,
    hint: Option<&'static str>,
}

fn lock_summary(
    check: &Result<WindowReport, NoReport>,
    language: Language,
    hint: bool,
    names: &str,
) -> LockSummary {
    let summary = match (check, language) {
        (Err(NoReport::NoWindowLayer), Language::ZhCn) => {
            "启动已上锁，本桌面没有窗口层，未能收起窗口".into()
        }
        (Err(NoReport::NoWindowLayer), Language::En) => {
            "Lock applied. This desktop has no window layer, so windows were not hidden".into()
        }
        (Err(NoReport::Unconfirmed), Language::ZhCn) => "已执行启动上锁，窗口状态未确认".into(),
        (Err(NoReport::Unconfirmed), Language::En) => {
            "Lock applied, but the window state was not confirmed".into()
        }
        (Ok(report), Language::ZhCn) => {
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
        (Ok(report), Language::En) => {
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
    };
    // Only a lock that actually concealed something raises the notification
    // concern. The hint tells the owner what the lock does not cover, so it
    // belongs next to the result rather than only in the documentation.
    let hint = if hint && matches!(check, Ok(report) if report.hidden > 0) {
        Some(match language {
            Language::ZhCn => "建议开启勿扰：受保护应用的通知仍会弹出，通知历史也能打开。",
            Language::En => {
                "Consider Do Not Disturb: protected apps' notifications still appear and their history stays accessible."
            }
        })
    } else {
        None
    };
    LockSummary { summary, hint }
}

fn notify_lock(
    handle: &Handle<TrayState>,
    summary: &str,
    hint: Option<&str>,
    concealed: bool,
    timeout_ms: i32,
) {
    let mut message = summary.to_string();
    if let Some(hint) = hint {
        message.push('\n');
        message.push_str(hint);
    }
    info!("{message}");
    let _ = handle.update(|tray| tray.last_lock = Some(summary.to_string()));
    let icon = if concealed {
        LOCKED_ICON
    } else {
        "shisa-tray-symbolic"
    };
    notify(&message, timeout_ms, icon);
}

/// Send one notification. Lock results are confirmations with no action,
/// and every lock gets its own popup so a repeated lock is confirmed again
/// after the previous result has expired. The focus-loss hint shares this
/// path.
fn notify(message: &str, timeout_ms: i32, icon: &str) -> bool {
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
    match result {
        Ok(_) => true,
        Err(e) => {
            warn!("notification: {e}");
            false
        }
    }
}

/// Explain the first focus-loss hide. The relock loop must not block on a
/// notification service that stopped answering, so the send runs on its own
/// thread. The flag is released when the notification cannot be delivered,
/// and a later hide tries again.
fn hint_relock_once() {
    if RELOCK_HINTED.swap(true, Ordering::Relaxed) {
        return;
    }
    if RELOCK_HINT_ATTEMPTS.fetch_add(1, Ordering::Relaxed) >= RELOCK_HINT_MAX_ATTEMPTS {
        // Stop spawning a thread per hide for a service that is not going to
        // show the hint. The flag stays set, so later hides stay silent.
        return;
    }
    thread::spawn(|| {
        // Do Not Disturb means the notification would be suppressed. Release
        // the one-time flag so a later hide can explain itself once DND is
        // off, instead of spending the hint on an invisible popup.
        if do_not_disturb() == Some(true) {
            RELOCK_HINTED.store(false, Ordering::Relaxed);
            return;
        }
        let message = match i18n::current() {
            Language::ZhCn => "窗口失焦后已自动隐藏。左键点击 Shisa 托盘图标可以打开恢复面板。",
            Language::En => {
                "The window was hidden after losing focus. Left-click the Shisa tray icon to open the restore panel."
            }
        };
        if !notify(message, NOTIFY_PROBLEM_TIMEOUT_MS, LOCKED_ICON) {
            RELOCK_HINTED.store(false, Ordering::Relaxed);
        }
    });
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
            let hidden = match manager.hide(&app) {
                Ok(()) => true,
                Err(e) => {
                    // A new agent can meet an older shisad that has no Hide. A
                    // lock is the old behavior and still conceals the window,
                    // even though it also drops the ticket.
                    debug!("relock {app}: {e}; falling back to a lock");
                    match manager.lock(&app) {
                        Ok(()) => true,
                        Err(e) => {
                            debug!("relock {app}: lock fallback failed: {e}");
                            false
                        }
                    }
                }
            };
            if hidden {
                hint_relock_once();
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
    // The absolute settings path keeps a user-writable directory earlier in
    // PATH from supplying the launcher. SHISA_SETTINGS_BIN overrides it for
    // development. Arch, Fedora, and Debian install the settings binary and
    // systemd-run at these paths.
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
    fn tray_menu_offers_restore_lock_and_settings() {
        let (actions, requests) = mpsc::channel();
        let mut tray = TrayState {
            hidden_windows: ((1, 1), HashMap::from([("org.kde.kate".into(), 2)])),
            hidden_ticketed: None,
            protected_count: 2,
            last_lock: Some("已收起 2 个窗口".into()),
            offline: false,
            ticket_grants: Default::default(),
            language: Language::ZhCn,
            actions,
        };
        let menu = tray.menu();
        let [
            MenuItem::Standard(restore),
            MenuItem::Standard(lock_all),
            MenuItem::Separator,
            MenuItem::Standard(settings),
        ] = menu.as_slice()
        else {
            panic!("expected restore, lock all and settings with a separator");
        };

        assert_eq!(lock_all.label, "锁定受保护应用");
        assert!(tray.tool_tip().description.contains("已隐藏 2 个窗口"));
        (restore.activate)(&mut tray);
        assert!(matches!(requests.try_recv(), Ok(Action::OpenRestore)));
        (lock_all.activate)(&mut tray);
        assert!(matches!(requests.try_recv(), Ok(Action::LockAll)));
        (settings.activate)(&mut tray);
        assert!(matches!(requests.try_recv(), Ok(Action::OpenSettings)));
    }

    #[test]
    fn lock_all_is_disabled_without_protected_apps() {
        let (actions, _requests) = mpsc::channel();
        let tray = TrayState {
            hidden_windows: Default::default(),
            hidden_ticketed: None,
            protected_count: 0,
            last_lock: None,
            offline: false,
            ticket_grants: Default::default(),
            language: Language::ZhCn,
            actions,
        };
        let menu = tray.menu();
        let MenuItem::Standard(lock_all) = &menu[1] else {
            panic!("expected lock all");
        };
        assert!(!lock_all.enabled);
        assert!(lock_all.label.contains("没有受保护的应用"));
    }

    #[test]
    fn lock_all_names_an_offline_daemon_in_the_tray_menu() {
        let (actions, _requests) = mpsc::channel();
        let tray = TrayState {
            hidden_windows: Default::default(),
            hidden_ticketed: None,
            protected_count: 2,
            last_lock: None,
            offline: true,
            ticket_grants: Default::default(),
            language: Language::En,
            actions,
        };
        let menu = tray.menu();
        let MenuItem::Standard(lock_all) = &menu[1] else {
            panic!("expected lock all");
        };
        assert!(!lock_all.enabled);
        assert_eq!(lock_all.label, "Lock protected apps (offline)");
    }

    #[test]
    fn the_tray_icon_shows_the_normal_locked_and_password_free_states() {
        let (actions, _requests) = mpsc::channel();
        let mut tray = TrayState {
            hidden_windows: Default::default(),
            hidden_ticketed: None,
            protected_count: 1,
            last_lock: None,
            offline: false,
            ticket_grants: Default::default(),
            language: Language::ZhCn,
            actions,
        };
        assert_eq!(tray.icon_name(), "shisa-tray-symbolic");
        tray.hidden_windows = ((1, 1), HashMap::from([("org.kde.kate".into(), 2)]));
        // An unconfirmed ticket state stays on the safe locked state.
        assert_eq!(tray.icon_name(), LOCKED_ICON);
        tray.hidden_ticketed = Some(0);
        assert_eq!(tray.icon_name(), LOCKED_ICON);
        tray.hidden_ticketed = Some(1);
        assert_eq!(tray.icon_name(), RESTORABLE_ICON);
        tray.offline = true;
        assert_eq!(tray.icon_name(), OFFLINE_ICON);
    }

    #[test]
    fn the_tray_tooltip_names_the_password_free_hidden_apps() {
        let (actions, _requests) = mpsc::channel();
        let mut tray = TrayState {
            hidden_windows: (
                (1, 1),
                HashMap::from([("org.kde.kate".into(), 2), ("org.kde.okular".into(), 1)]),
            ),
            hidden_ticketed: Some(1),
            protected_count: 2,
            last_lock: None,
            offline: false,
            ticket_grants: Default::default(),
            language: Language::ZhCn,
            actions,
        };
        let description = tray.tool_tip().description;
        assert!(description.contains("已隐藏 3 个窗口"));
        assert!(description.contains("其中 1 个应用可免密还原"));
        tray.hidden_ticketed = Some(0);
        assert!(tray.tool_tip().description.contains("恢复需要应用锁密码"));
        tray.language = Language::En;
        tray.hidden_ticketed = Some(2);
        assert!(
            tray.tool_tip()
                .description
                .contains("2 apps can restore without the app-lock password")
        );
    }

    #[test]
    fn the_tray_tooltip_does_not_claim_a_password_while_the_ticket_state_is_unknown() {
        let (actions, _requests) = mpsc::channel();
        let mut tray = TrayState {
            hidden_windows: ((1, 1), HashMap::from([("org.kde.kate".into(), 2)])),
            hidden_ticketed: None,
            protected_count: 1,
            last_lock: None,
            offline: false,
            ticket_grants: Default::default(),
            language: Language::ZhCn,
            actions,
        };
        let description = tray.tool_tip().description;
        assert!(description.contains("已隐藏 2 个窗口"));
        assert!(description.contains("还原凭证状态未确认"));
        assert!(!description.contains("恢复需要应用锁密码"));
        tray.language = Language::En;
        let description = tray.tool_tip().description;
        assert!(description.contains("The restore ticket state is unconfirmed"));
        assert!(!description.contains("Restoring asks for the app-lock password"));
    }

    #[test]
    fn only_ticketed_hidden_apps_count_as_password_free() {
        let apps: Vec<AppRow> = vec![
            ("org.kde.kate".into(), true, Vec::new(), Vec::new()),
            ("org.kde.okular".into(), true, Vec::new(), Vec::new()),
            ("org.kde.ark".into(), false, Vec::new(), Vec::new()),
        ];
        let hidden = HashMap::from([("org.kde.kate".into(), 2), ("org.kde.ark".into(), 1)]);
        assert_eq!(ticketed_hidden(&apps, &hidden), 1);
        assert_eq!(ticketed_hidden(&apps, &HashMap::new()), 0);
    }

    #[test]
    fn a_ticket_poll_keeps_the_safe_state_when_the_daemon_is_unknown() {
        let hidden = HashMap::from([("org.kde.kate".into(), 2)]);
        let apps: Vec<AppRow> = vec![
            ("org.kde.kate".into(), true, Vec::new(), Vec::new()),
            ("org.kde.ark".into(), false, Vec::new(), Vec::new()),
        ];
        // shisad offline: leave the last value alone.
        assert_eq!(ticket_poll(&hidden, true, None), TicketPoll::Skip);
        // No hidden window needs no bus call.
        assert_eq!(
            ticket_poll(&HashMap::new(), false, None),
            TicketPoll::Confirmed(0)
        );
        // A successful read counts only ticketed hidden apps.
        assert_eq!(
            ticket_poll(&hidden, false, Some(Ok(apps))),
            TicketPoll::Confirmed(1)
        );
        // A failed or missing read must not claim the password is needed.
        assert_eq!(
            ticket_poll(
                &hidden,
                false,
                Some(Err(zbus::Error::Failure("offline".into())))
            ),
            TicketPoll::Unconfirmed
        );
        assert_eq!(ticket_poll(&hidden, false, None), TicketPoll::Unconfirmed);
    }

    fn tray_without_hidden() -> TrayState {
        let (actions, _requests) = mpsc::channel();
        TrayState {
            hidden_windows: Default::default(),
            hidden_ticketed: None,
            protected_count: 1,
            last_lock: None,
            offline: false,
            ticket_grants: Default::default(),
            language: Language::ZhCn,
            actions,
        }
    }

    #[test]
    fn a_hidden_report_publishes_its_ticket_count_in_the_same_update() {
        let mut tray = tray_without_hidden();
        let counts = HashMap::from([("org.kde.motrix".into(), 1)]);
        // The focus-loss hide kept the ticket, so the ring goes from green
        // straight to amber instead of showing the unconfirmed blue first.
        assert!(apply_hidden_windows(
            &mut tray,
            (1, 1),
            counts.clone(),
            Some(1)
        ));
        assert_eq!(tray.icon_name(), RESTORABLE_ICON);
        // A report that could not be matched to tickets keeps the fallback,
        // and a stale report changes nothing.
        assert!(apply_hidden_windows(
            &mut tray,
            (1, 2),
            counts.clone(),
            None
        ));
        assert_eq!(tray.icon_name(), LOCKED_ICON);
        assert!(!apply_hidden_windows(&mut tray, (1, 1), counts, Some(1)));
        assert_eq!(tray.icon_name(), LOCKED_ICON);
    }

    #[test]
    fn a_ticket_grant_holds_the_previous_color_until_its_window_returns() {
        let mut tray = tray_without_hidden();
        let counts = HashMap::from([("org.kde.motrix".into(), 1)]);
        // The locked blue before the user authenticates the restore.
        apply_hidden_windows(&mut tray, (1, 1), counts.clone(), Some(0));
        assert_eq!(tray.icon_name(), LOCKED_ICON);
        hold_ticket_grant(&mut tray, "org.kde.motrix", Instant::now());
        // The grant's read would count the app as amber, but KWin still
        // reports its window hidden.
        apply_ticket_poll(&mut tray, TicketPoll::Confirmed(1));
        assert_eq!(tray.icon_name(), LOCKED_ICON);
        // The report that brings the window back ends the hold and publishes
        // the new count.
        apply_hidden_windows(&mut tray, (1, 2), HashMap::new(), Some(0));
        assert!(tray.ticket_grants.is_empty());
        assert_eq!(tray.icon_name(), "shisa-tray-symbolic");
    }

    #[test]
    fn an_expired_ticket_grant_stops_holding_the_color() {
        let mut tray = tray_without_hidden();
        let counts = HashMap::from([("org.kde.motrix".into(), 1)]);
        apply_hidden_windows(&mut tray, (1, 1), counts, Some(0));
        // The window never came back. The hold expires instead of freezing
        // the color forever.
        tray.ticket_grants.insert(
            "org.kde.motrix".into(),
            Instant::now() - Duration::from_secs(1),
        );
        apply_ticket_poll(&mut tray, TicketPoll::Confirmed(1));
        assert_eq!(tray.icon_name(), RESTORABLE_ICON);
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
        let none: Result<WindowReport, NoReport> = Ok(report(0, 0, &[]));
        let full: Result<WindowReport, NoReport> = Ok(report(2, 2, &[]));
        let partial: Result<WindowReport, NoReport> = Ok(report(3, 2, &[("org.kde.kate", 1)]));
        let unconfirmed: Result<WindowReport, NoReport> = Err(NoReport::Unconfirmed);
        let no_layer: Result<WindowReport, NoReport> = Err(NoReport::NoWindowLayer);
        assert!(
            lock_summary(&none, Language::ZhCn, false, "")
                .summary
                .contains("未匹配")
        );
        assert!(
            lock_summary(&full, Language::ZhCn, false, "")
                .summary
                .contains("已收起 2")
        );
        assert!(
            lock_summary(&partial, Language::ZhCn, false, "app.one")
                .summary
                .contains("未确认：app.one")
        );
        assert_eq!(
            lock_summary(
                &Ok::<_, NoReport>(report(2, 1, &[])),
                Language::ZhCn,
                false,
                ""
            )
            .summary,
            lock_summary(&unconfirmed, Language::ZhCn, false, "").summary
        );
        assert!(
            lock_summary(&unconfirmed, Language::ZhCn, false, "")
                .summary
                .contains("未确认")
        );
        // A desktop without KWin is a different result from a check that did
        // not come back. The previous message called both unconfirmed.
        assert!(
            lock_summary(&no_layer, Language::ZhCn, false, "")
                .summary
                .contains("没有窗口层")
        );
        assert!(
            !lock_summary(&no_layer, Language::ZhCn, false, "")
                .summary
                .contains("未确认")
        );

        assert!(
            lock_summary(&none, Language::En, false, "")
                .summary
                .contains("no protected window")
        );
        assert!(
            lock_summary(&full, Language::En, false, "")
                .summary
                .contains("hid 2")
        );
        assert!(
            lock_summary(&partial, Language::En, false, "app.one")
                .summary
                .contains("Not confirmed: app.one")
        );
        assert_eq!(
            lock_summary(
                &Ok::<_, NoReport>(report(2, 1, &[])),
                Language::En,
                false,
                ""
            )
            .summary,
            lock_summary(&unconfirmed, Language::En, false, "").summary
        );
        assert!(
            lock_summary(&unconfirmed, Language::En, false, "")
                .summary
                .contains("not confirmed")
        );
        assert!(
            lock_summary(&no_layer, Language::En, false, "")
                .summary
                .contains("no window layer")
        );
        assert!(
            !lock_summary(&no_layer, Language::En, false, "")
                .summary
                .contains("not confirmed")
        );
    }

    #[test]
    fn the_dnd_hint_follows_a_lock_that_hid_something() {
        let full: Result<WindowReport, NoReport> = Ok(report(2, 2, &[]));
        let hinted = lock_summary(&full, Language::ZhCn, true, "");
        // The hint rides on the notification only. The tooltip keeps the
        // summary, which is what `notify_lock` stores in `last_lock`.
        assert!(hinted.summary.contains("已收起 2"));
        assert!(hinted.hint.is_some_and(|hint| hint.contains("勿扰")));
        assert!(
            lock_summary(&full, Language::ZhCn, false, "")
                .hint
                .is_none()
        );
        // Nothing was hidden, so notifications from hidden apps are not the
        // concern this hint addresses.
        let none: Result<WindowReport, NoReport> = Ok(report(0, 0, &[]));
        assert!(lock_summary(&none, Language::ZhCn, true, "").hint.is_none());
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
