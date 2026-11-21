//! shisad's side of the system bus (docs/subsystems/configuration.md, docs/subsystems/authentication.md).
//!
//! The bus thread owns the connection, the served object and the registry of
//! session agents. Method calls reach the shared state directly. Everything
//! the event loop has to send out goes through the channel, so the fanotify
//! loop never waits for the bus.
//!
//! The interface methods are async on purpose. A blocking call made from a
//! method on the same connection would stop the executor that has to deliver
//! the reply, which deadlocks. The caller's uid lookup is exactly such a call.

use crate::activity;
use crate::diag::{self, Diag};
use crate::events::{Event, Events};
use crate::policy::{Change, State};
use futures_channel::oneshot;
use shisa_core::config::valid_app_id;
use shisa_core::dbus::{AppRow, BUS_NAME, DiagRow, INTERFACE, LaunchRow, OBJECT_PATH};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};
use tracing::{debug, error, info};
use zbus::Connection as AsyncConnection;
use zbus::blocking::Connection;
use zbus::message::Header;

pub type Shared = Arc<Mutex<State>>;

pub enum Msg {
    /// A ticket of this user was issued or dropped.
    StateChanged {
        uid: u32,
        app: String,
        unlocked: bool,
    },
    /// The settings change is done, or refused. Answers the waiting method call.
    ChangeDone { id: u64, result: Result<(), String> },
    /// The screen of this user unlocked. KWin ignored the window changes made
    /// while it was locked, so the agent has to reapply the gate.
    SessionUnlocked { uid: u32 },
}

type Waiting = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<(), String>>>>>;

/// Cap on threads doing desktop scans for D-Bus calls. The calls are cheap
/// and user-triggered, but without a cap any local user could queue them and
/// grow the root process without bound.
const MAX_WORKERS: usize = 8;
const MAX_WORKERS_PER_UID: usize = 2;

#[derive(Default)]
struct Limits {
    total: usize,
    per_uid: HashMap<u32, usize>,
}

/// One occupied worker slot, released on drop.
struct Slot {
    limits: Arc<Mutex<Limits>>,
    uid: u32,
}

impl Slot {
    fn acquire(limits: Arc<Mutex<Limits>>, uid: u32) -> Option<Self> {
        let mut state = limits.lock().unwrap();
        if state.total >= MAX_WORKERS
            || state.per_uid.get(&uid).copied().unwrap_or(0) >= MAX_WORKERS_PER_UID
        {
            return None;
        }
        state.total += 1;
        *state.per_uid.entry(uid).or_insert(0) += 1;
        drop(state);
        Some(Slot { limits, uid })
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut state = self.limits.lock().unwrap();
        state.total -= 1;
        if let Some(count) = state.per_uid.get_mut(&self.uid) {
            *count -= 1;
            if *count == 0 {
                state.per_uid.remove(&self.uid);
            }
        }
    }
}

/// Connect to the system bus, claim the name and serve the interface.
/// Answers that the loop must act on go through `events`, which wakes it.
///
/// The connection is built before the thread starts, so a second shisad that
/// cannot claim the name fails here and never starts the polkit flow.
pub fn spawn(
    state: Shared,
    events: Arc<Events>,
    activity: Arc<activity::Log>,
    diag: Arc<Diag>,
) -> Result<mpsc::Sender<Msg>, String> {
    let (tx, rx) = mpsc::channel::<Msg>();
    let agents: Arc<Mutex<HashMap<u32, String>>> = Arc::default();
    let waiting: Waiting = Arc::default();
    let iface = ManagerIface {
        indexes: Arc::default(),
        activity,
        diag,
        state,
        agents: agents.clone(),
        events: events.clone(),
        waiting: waiting.clone(),
        limits: Arc::default(),
    };
    let conn = (|| {
        zbus::blocking::connection::Builder::system()?
            .name(BUS_NAME)?
            .serve_at(OBJECT_PATH, iface)?
            .build()
    })()
    .map_err(|e| e.to_string())?;
    thread::Builder::new()
        .name("dbus".into())
        .spawn(move || run(conn, rx, agents, waiting, events))
        .map_err(|e| e.to_string())?;
    Ok(tx)
}

fn run(
    conn: Connection,
    rx: mpsc::Receiver<Msg>,
    agents: Arc<Mutex<HashMap<u32, String>>>,
    waiting: Waiting,
    events: Arc<Events>,
) {
    info!("serving on the system bus");
    loop {
        let msg = match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(msg) => msg,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if conn.is_closed() {
                    error!("system bus connection closed");
                    events.push(Event::BusLost);
                    return;
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        match msg {
            Msg::StateChanged { uid, app, unlocked } => {
                // Addressed to the user's agent. A broadcast would tell every
                // account on the machine which apps this user protects.
                let Some(dest) = agents.lock().unwrap().get(&uid).cloned() else {
                    continue;
                };
                if let Err(e) = conn.emit_signal(
                    Some(dest.as_str()),
                    OBJECT_PATH,
                    INTERFACE,
                    "StateChanged",
                    &(app, unlocked),
                ) {
                    debug!("StateChanged: {e}");
                }
            }
            Msg::ChangeDone { id, result } => {
                if let Some(tx) = waiting.lock().unwrap().remove(&id) {
                    let _ = tx.send(result);
                }
            }
            Msg::SessionUnlocked { uid } => {
                let Some(dest) = agents.lock().unwrap().get(&uid).cloned() else {
                    continue;
                };
                if let Err(e) = conn.emit_signal(
                    Some(dest.as_str()),
                    OBJECT_PATH,
                    INTERFACE,
                    "SessionUnlocked",
                    &(),
                ) {
                    debug!("SessionUnlocked: {e}");
                }
            }
        }
    }
}

/// How long a scan of the desktop files is reused. The settings window asks
/// for one app after another while the user clicks through the list.
const INDEX_AGE: Duration = Duration::from_secs(5);

type IndexCache = Arc<Mutex<HashMap<u32, (Instant, Arc<shisa_core::desktop::Index>)>>>;

struct ManagerIface {
    indexes: IndexCache,
    activity: Arc<activity::Log>,
    diag: Arc<Diag>,
    state: Shared,
    agents: Arc<Mutex<HashMap<u32, String>>>,
    events: Arc<Events>,
    waiting: Waiting,
    limits: Arc<Mutex<Limits>>,
}

#[zbus::interface(name = "io.github.RiverOnVenus.Shisa.Manager")]
impl ManagerIface {
    async fn list_apps(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &AsyncConnection,
    ) -> zbus::fdo::Result<Vec<AppRow>> {
        let uid = caller_uid(conn, &hdr).await?;
        Ok(self.state.lock().unwrap().list_apps(uid, Instant::now()))
    }

    /// The caller's recent launch decisions, oldest first, and the totals
    /// since the daemon started: requests, allowed, denied.
    async fn recent_launches(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &AsyncConnection,
    ) -> zbus::fdo::Result<(Vec<LaunchRow>, u64, u64, u64)> {
        let uid = caller_uid(conn, &hdr).await?;
        Ok(self.activity.recent(uid))
    }

    /// The running daemon's own numbers, and whether the app-lock account has
    /// a password yet. The app and path counts are the caller's own.
    async fn diagnostics(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &AsyncConnection,
    ) -> zbus::fdo::Result<DiagRow> {
        let uid = caller_uid(conn, &hdr).await?;
        let (apps, marked, known_name) = {
            let state = self.state.lock().unwrap();
            let user = state.policy().users.get(&uid);
            let apps = user.map(|u| u.apps.len() as u64).unwrap_or(0);
            let marked = user
                .map(|u| u.apps.iter().map(|a| a.effective.len() as u64).sum())
                .unwrap_or(0);
            let name = user.map(|u| u.name.clone());
            (apps, marked, name)
        };
        // The account lookup and the /etc/shadow read block like any file
        // access, so both run on a worker rather than the bus executor. A
        // user with no policy file still gets the path it would have.
        let (ready, name) = self
            .blocking(uid, move || {
                let name = known_name.or_else(|| shisa_core::users::name(uid));
                (diag::admin_ready(), name)
            })
            .await?;
        let config = name.map(|n| self.diag.config_path(&n)).unwrap_or_default();
        Ok((
            self.diag.version().to_string(),
            self.diag.started(),
            self.diag.reloaded(),
            self.diag.failed(),
            apps,
            marked,
            config,
            ready,
        ))
    }

    async fn lock(
        &self,
        app_id: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &AsyncConnection,
    ) -> zbus::fdo::Result<()> {
        let uid = caller_uid(conn, &hdr).await?;
        let cleared: Vec<String> = {
            let mut state = self.state.lock().unwrap();
            if app_id.is_empty() {
                state.clear_tickets(uid)
            } else {
                state.lock_app(uid, app_id).into_iter().collect()
            }
        };
        let agent = self.agents.lock().unwrap().get(&uid).cloned();
        if let Some(agent) = agent {
            for app in cleared {
                state_changed(conn, &agent, app, false).await;
            }
        }
        Ok(())
    }

    /// Authenticate for an app and leave a ticket, as `shisa unlock` does.
    async fn unlock(
        &self,
        app_id: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &AsyncConnection,
    ) -> zbus::fdo::Result<()> {
        let uid = caller_uid(conn, &hdr).await?;
        if !valid_app_id(app_id) {
            return Err(zbus::fdo::Error::InvalidArgs(format!(
                "invalid app id {app_id:?}"
            )));
        }
        if !self.state.lock().unwrap().is_protected(uid, app_id) {
            return Err(zbus::fdo::Error::Failed(format!(
                "{app_id} is not protected"
            )));
        }
        let sender = hdr
            .sender()
            .ok_or_else(|| zbus::fdo::Error::Failed("no sender".into()))?
            .to_string();
        let id = self.events.next_id();
        let (tx, rx) = oneshot::channel();
        self.waiting.lock().unwrap().insert(id, tx);
        self.events.push(Event::UnlockRequest {
            id,
            uid,
            app: app_id.to_string(),
            sender,
        });
        match rx.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(message)) => Err(zbus::fdo::Error::Failed(message)),
            Err(_) => Err(zbus::fdo::Error::Failed("request dropped".into())),
        }
    }

    /// Apps whose windows may show. This is not the ticket set: a ticket
    /// expiring only means the next launch needs authentication, while an
    /// open window stays visible until a lock (docs/subsystems/window-management.md).
    async fn window_states(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &AsyncConnection,
    ) -> zbus::fdo::Result<Vec<String>> {
        let uid = caller_uid(conn, &hdr).await?;
        Ok(self.state.lock().unwrap().window_allowed(uid))
    }

    /// Installed applications for the settings list: id, name, icon.
    async fn list_installed(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &AsyncConnection,
    ) -> zbus::fdo::Result<Vec<(String, String, String)>> {
        let uid = caller_uid(conn, &hdr).await?;
        let rows = self
            .blocking(uid, move || {
                let Some(env) = shisa_core::desktop::Env::for_user(uid) else {
                    return Err("no home directory".to_string());
                };
                Ok(shisa_core::desktop::installed(&env)
                    .into_iter()
                    .map(|a| (a.id, a.name, a.icon))
                    .collect())
            })
            .await?;
        rows.map_err(zbus::fdo::Error::Failed)
    }

    /// Resolved paths and the caveats that go with them.
    async fn resolve_app(
        &self,
        app_id: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &AsyncConnection,
    ) -> zbus::fdo::Result<(Vec<String>, Vec<String>)> {
        let uid = caller_uid(conn, &hdr).await?;
        let cache = self.indexes.clone();
        let app_id = app_id.to_string();
        let found = self
            .blocking(uid, move || {
                let Some(env) = shisa_core::desktop::Env::for_user(uid) else {
                    return Err("no home directory".to_string());
                };
                let index = index_for(&cache, uid, &env);
                let found = shisa_core::desktop::resolve_in(&index, &app_id, &env)
                    .map_err(|e| e.to_string())?;
                let language = env.language();
                let paths = found
                    .paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect();
                let notes = found.notes.iter().map(|n| n.message(language)).collect();
                Ok((paths, notes))
            })
            .await?;
        found.map_err(zbus::fdo::Error::Failed)
    }

    /// Effective grace period, autostart flag, protection state and the
    /// unfocus relock delay.
    async fn app_config(
        &self,
        app_id: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &AsyncConnection,
    ) -> zbus::fdo::Result<(u64, bool, bool, u64)> {
        let uid = caller_uid(conn, &hdr).await?;
        Ok(self.state.lock().unwrap().app_config(uid, app_id))
    }

    /// Per-app unfocus relock delays for the session agent. Apps with the
    /// feature off are left out.
    async fn relock_seconds(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &AsyncConnection,
    ) -> zbus::fdo::Result<Vec<(String, u64)>> {
        let uid = caller_uid(conn, &hdr).await?;
        Ok(self.state.lock().unwrap().relock_seconds(uid))
    }

    /// Global grace period, whether the screen lock drops tickets, and the
    /// wait limit for the settings window.
    async fn global_config(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &AsyncConnection,
    ) -> zbus::fdo::Result<(u64, bool, u64)> {
        let uid = caller_uid(conn, &hdr).await?;
        Ok(self.state.lock().unwrap().global_config(uid))
    }

    /// Mark an app as protected or not. The change only happens after the
    /// user authenticated (docs/subsystems/configuration.md), which the event loop arranges.
    async fn set_protected(
        &self,
        app_id: &str,
        on: bool,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &AsyncConnection,
    ) -> zbus::fdo::Result<()> {
        let uid = caller_uid(conn, &hdr).await?;
        if !valid_app_id(app_id) {
            return Err(zbus::fdo::Error::InvalidArgs(format!(
                "invalid app id {app_id:?}"
            )));
        }
        let sender = hdr
            .sender()
            .ok_or_else(|| zbus::fdo::Error::Failed("no sender".into()))?
            .to_string();
        self.change(uid, app_id, Change::Protected(on), sender)
            .await
    }

    /// Change one option. An empty app id addresses the user's global
    /// settings. Supported keys: grace_seconds, autostart, paths,
    /// clear_on_screen_lock, prompt_timeout_seconds.
    async fn set_option(
        &self,
        app_id: &str,
        key: &str,
        value: zbus::zvariant::Value<'_>,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &AsyncConnection,
    ) -> zbus::fdo::Result<()> {
        let uid = caller_uid(conn, &hdr).await?;
        if !app_id.is_empty() && !valid_app_id(app_id) {
            return Err(zbus::fdo::Error::InvalidArgs(format!(
                "invalid app id {app_id:?}"
            )));
        }
        // An empty app id means the global settings, so the key has to match:
        // otherwise the authentication box would name one app while the change
        // lands in the global section (docs/subsystems/configuration.md).
        let global_only = matches!(key, "clear_on_screen_lock" | "prompt_timeout_seconds");
        let app_only = matches!(key, "autostart" | "paths" | "relock_after_unfocus_seconds");
        if (app_id.is_empty() && app_only) || (!app_id.is_empty() && global_only) {
            return Err(zbus::fdo::Error::InvalidArgs(format!(
                "option {key} does not apply to {}",
                if app_id.is_empty() {
                    "the global settings"
                } else {
                    "an application"
                }
            )));
        }
        let change = parse_option(key, &value)?;
        crate::load::check_range(&change).map_err(zbus::fdo::Error::InvalidArgs)?;
        // An option of an app the user does not protect would fail after the
        // authentication. Refuse it before the box names an app that cannot
        // take the change.
        if !app_id.is_empty() && !self.state.lock().unwrap().is_protected(uid, app_id) {
            return Err(zbus::fdo::Error::Failed(format!(
                "{app_id} is not protected"
            )));
        }
        let sender = hdr
            .sender()
            .ok_or_else(|| zbus::fdo::Error::Failed("no sender".into()))?
            .to_string();
        self.change(uid, app_id, change, sender).await
    }

    async fn register_agent(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &AsyncConnection,
    ) -> zbus::fdo::Result<()> {
        let uid = caller_uid(conn, &hdr).await?;
        let sender = hdr
            .sender()
            .ok_or_else(|| zbus::fdo::Error::Failed("no sender".into()))?
            .to_string();
        self.agents.lock().unwrap().insert(uid, sender);
        debug!(uid, "agent registered");
        Ok(())
    }
}

impl ManagerIface {
    /// Run filesystem and NSS work off the zbus executor thread. A method
    /// that blocks there also holds up the reply for every other call on the
    /// connection, including the `caller_uid` lookup. The slot bounds how
    /// many workers one user can hold.
    async fn blocking<T: Send + 'static>(
        &self,
        uid: u32,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> zbus::fdo::Result<T> {
        let slot = Slot::acquire(self.limits.clone(), uid)
            .ok_or_else(|| zbus::fdo::Error::Failed("too many requests".into()))?;
        let (tx, rx) = oneshot::channel();
        let spawned = thread::Builder::new()
            .name("dbus-work".into())
            .spawn(move || {
                let _slot = slot;
                let _ = tx.send(work());
            });
        if let Err(e) = spawned {
            return Err(zbus::fdo::Error::Failed(format!("worker: {e}")));
        }
        rx.await
            .map_err(|_| zbus::fdo::Error::Failed("worker exited".into()))
    }

    /// Hand the change to the event loop and wait for the result. The loop
    /// asks the agent to authenticate this one change and then writes the
    /// configuration.
    async fn change(
        &self,
        uid: u32,
        app: &str,
        change: Change,
        sender: String,
    ) -> zbus::fdo::Result<()> {
        let id = self.events.next_id();
        let (tx, rx) = oneshot::channel();
        self.waiting.lock().unwrap().insert(id, tx);
        let request = Event::ChangeRequest {
            id,
            uid,
            app: app.to_string(),
            change,
            sender,
        };
        self.events.push(request);
        match rx.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(message)) => Err(zbus::fdo::Error::Failed(message)),
            Err(_) => Err(zbus::fdo::Error::Failed("request dropped".into())),
        }
    }
}

/// The user's desktop file index, rebuilt when it is older than
/// [`INDEX_AGE`]. Runs on the blocking worker, never on the bus executor.
fn index_for(
    cache: &IndexCache,
    uid: u32,
    env: &shisa_core::desktop::Env,
) -> Arc<shisa_core::desktop::Index> {
    let mut cache = cache.lock().unwrap();
    cache.retain(|_, (built, _)| built.elapsed() < INDEX_AGE);
    let entry = cache.entry(uid).or_insert_with(|| {
        (
            Instant::now(),
            Arc::new(shisa_core::desktop::Index::scan(env)),
        )
    });
    entry.1.clone()
}

fn parse_option(key: &str, value: &zbus::zvariant::Value<'_>) -> zbus::fdo::Result<Change> {
    use zbus::zvariant::Value;
    let invalid = || zbus::fdo::Error::InvalidArgs(format!("unsupported option {key}"));
    let seconds = |value: &Value<'_>| match value {
        Value::U64(v) => Some(*v),
        Value::U32(v) => Some(*v as u64),
        Value::I64(v) if *v >= 0 => Some(*v as u64),
        _ => None,
    };
    match key {
        "grace_seconds" => seconds(value).map(Change::Grace).ok_or_else(invalid),
        "prompt_timeout_seconds" => seconds(value)
            .map(Change::PromptTimeout)
            .ok_or_else(invalid),
        "autostart" => match value {
            Value::Bool(on) => Ok(Change::Autostart(*on)),
            Value::Str(s) => match s.as_str() {
                "allow" => Ok(Change::Autostart(true)),
                "prompt" => Ok(Change::Autostart(false)),
                _ => Err(invalid()),
            },
            _ => Err(invalid()),
        },
        "relock_after_unfocus_seconds" => seconds(value).map(Change::Relock).ok_or_else(invalid),
        "clear_on_screen_lock" => match value {
            Value::Bool(on) => Ok(Change::ClearOnLock(*on)),
            _ => Err(invalid()),
        },
        "paths" => {
            let Value::Array(array) = value else {
                return Err(invalid());
            };
            let mut paths = Vec::new();
            for item in array.iter() {
                let Ok(path) = item.downcast_ref::<&str>() else {
                    return Err(invalid());
                };
                paths.push(std::path::PathBuf::from(path));
            }
            Ok(Change::Paths(paths))
        }
        _ => Err(invalid()),
    }
}

async fn state_changed(conn: &AsyncConnection, agent: &str, app: String, unlocked: bool) {
    let sent = conn
        .emit_signal(
            Some(agent),
            OBJECT_PATH,
            INTERFACE,
            "StateChanged",
            &(app, unlocked),
        )
        .await;
    if let Err(e) = sent {
        debug!("StateChanged: {e}");
    }
}

/// The uid behind a bus connection. The bus alone decides this, so the caller
/// cannot claim to be someone else.
async fn caller_uid(conn: &AsyncConnection, hdr: &Header<'_>) -> zbus::fdo::Result<u32> {
    let sender = hdr
        .sender()
        .ok_or_else(|| zbus::fdo::Error::Failed("no sender".into()))?;
    let reply = conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetConnectionUnixUser",
            &(sender.as_str(),),
        )
        .await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
    reply
        .body()
        .deserialize::<u32>()
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_slots_are_capped() {
        let limits = Arc::new(Mutex::new(Limits::default()));
        let a = Slot::acquire(limits.clone(), 1).unwrap();
        let b = Slot::acquire(limits.clone(), 1).unwrap();
        assert!(Slot::acquire(limits.clone(), 1).is_none());
        let other = Slot::acquire(limits.clone(), 2).unwrap();
        drop(a);
        let c = Slot::acquire(limits.clone(), 1).unwrap();
        drop((b, c, other));
        let state = limits.lock().unwrap();
        assert_eq!(state.total, 0);
        assert!(state.per_uid.is_empty());
    }

    #[test]
    fn names_match_the_design() {
        assert_eq!(BUS_NAME, "io.github.RiverOnVenus.Shisa");
        assert_eq!(OBJECT_PATH, "/io/github/RiverOnVenus/Shisa/Manager");
    }
}
