//! Policy, tickets, instances and the exec decision (docs/subsystems/launch-gate.md, docs/subsystems/authentication.md).
//!
//! Nothing here touches the system directly. Process facts come through
//! [`Procs`] and time is passed in, so the whole decision is unit tested.

use crate::procfs::{FileId, ProcId, Procs};
use shisa_core::dbus::AppRow;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Used when an app has no per-app entry (shown by `app_config` for apps the
/// user has not protected yet) and for a user without a config file.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(shisa_core::config::DEFAULT_GRACE_SECONDS);
pub const DEFAULT_PROMPT_TIMEOUT: Duration =
    Duration::from_secs(shisa_core::config::DEFAULT_PROMPT_TIMEOUT_SECONDS);
/// Autostart launches count as exempt for this long after the session began
/// (docs/subsystems/launch-gate.md).
pub const AUTOSTART_WINDOW: Duration = Duration::from_secs(30);
/// How far up the process tree the instance check walks.
const MAX_ANCESTORS: usize = 64;

#[derive(Debug, Clone)]
pub struct AppPolicy {
    pub id: String,
    pub grace: Duration,
    pub autostart: bool,
    /// How long an allowed window may sit unfocused before it is hidden.
    /// Zero turns this off (docs/subsystems/window-management.md).
    pub relock: Duration,
    /// Paths from the config, including ones that could not be marked.
    pub paths: Vec<PathBuf>,
    /// The subset of `paths` that was marked at the last load. What the
    /// settings window shows as actually in effect.
    pub effective: Vec<PathBuf>,
    pub files: Vec<FileId>,
}

#[derive(Debug, Clone)]
pub struct UserPolicy {
    pub name: String,
    pub prompt_timeout: Duration,
    /// Grace period for apps without their own entry, shown by `app_config`
    /// for apps the user has not protected yet.
    pub global_grace: Duration,
    /// Drop the user's tickets when the screen locks (docs/subsystems/launch-gate.md, docs/subsystems/configuration.md).
    pub clear_on_lock: bool,
    pub apps: Vec<AppPolicy>,
}

/// A settings change, authenticated before it is written (docs/subsystems/configuration.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Protected(bool),
    /// Grace period of one app, or of the user when the app id is empty.
    Grace(u64),
    Autostart(bool),
    Paths(Vec<PathBuf>),
    ClearOnLock(bool),
    PromptTimeout(u64),
    /// Unfocus relock delay of one app, zero turns it off.
    Relock(u64),
}

#[derive(Debug, Clone, Default)]
pub struct Policy {
    pub users: HashMap<u32, UserPolicy>,
}

impl Policy {
    pub fn files(&self) -> HashSet<FileId> {
        let apps = self.users.values().flat_map(|u| &u.apps);
        apps.flat_map(|a| a.files.iter().copied()).collect()
    }

    fn covers(&self, file: FileId) -> bool {
        self.users
            .values()
            .flat_map(|u| &u.apps)
            .any(|a| a.files.contains(&file))
    }

    fn app(&self, uid: u32, id: &str) -> Option<&AppPolicy> {
        self.users.get(&uid)?.apps.iter().find(|a| a.id == id)
    }
}

/// Why an exec request was answered the way it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// The file is not a protected path of anyone.
    Unprotected,
    /// The caller's user does not protect this app.
    OtherUser,
    /// The caller already runs the app's own image.
    SelfExec,
    /// The caller descends from a running instance of the app.
    Instance,
    /// A valid ticket covered the launch.
    Ticket,
    /// The session just started and the app allows autostart launches.
    Autostart,
    /// The caller vanished before it could be identified.
    CallerGone,
    /// No ticket. The request has to be authenticated.
    NeedAuth,
}

impl Why {
    pub fn allows(self) -> bool {
        !matches!(self, Why::CallerGone | Why::NeedAuth)
    }
}

/// Answer to one exec request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub why: Why,
    /// Set for [`Why::NeedAuth`]: the user to authenticate and the app to
    /// issue the ticket for.
    pub auth: Option<(u32, String)>,
    /// The caller's user and the app of this user the decision was about,
    /// when it could name one. The activity log shows only the caller's own
    /// launches, so a decision about another user's app carries neither.
    pub uid: Option<u32>,
    pub app: Option<String>,
    /// Tickets that disappeared while answering this request: a one-shot
    /// ticket consumed by the launch, or an expired ticket found in the
    /// table. The session agent has to be told those apps are locked again.
    pub lost_tickets: Vec<String>,
}

impl Decision {
    fn allow(why: Why) -> Self {
        Decision {
            why,
            auth: None,
            uid: None,
            app: None,
            lost_tickets: Vec::new(),
        }
    }

    fn allow_app(uid: u32, app: &str, why: Why) -> Self {
        Decision {
            why,
            auth: None,
            uid: Some(uid),
            app: Some(app.to_string()),
            lost_tickets: Vec::new(),
        }
    }
}

struct Ticket {
    expires: Instant,
    /// Issued with a grace period of 0: covers one launch only.
    one_shot: bool,
}

/// What asking for a ticket found.
enum TicketUse {
    None,
    /// A valid ticket covered the launch. `one_shot` means the lookup
    /// consumed it, so the app is locked again.
    Valid {
        one_shot: bool,
    },
    /// A ticket was there but had expired. It is removed and the app is
    /// locked.
    Expired,
}

/// What a successful authentication is for. A held exec is allowed by the
/// event loop right away, while `shisa unlock` needs a ticket for a later
/// launch.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GrantUse {
    ForLater,
    HeldExec,
}

type Key = (u32, String);

#[derive(Default)]
pub struct State {
    policy: Policy,
    tickets: HashMap<Key, Ticket>,
    instances: HashMap<Key, Vec<ProcId>>,
    /// Apps whose windows may stay visible. Unlike tickets, this survives the
    /// grace period. A screen lock or a manual `Lock` clears it, and an
    /// authenticated launch or window activation sets it (docs/subsystems/window-management.md).
    window_allowed: HashMap<u32, HashSet<String>>,
    /// When the user's graphical session began, for the autostart window.
    session_start: HashMap<u32, Instant>,
}

impl State {
    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// Replace the policy and forget state of apps that are no longer in it.
    pub fn set_policy(&mut self, policy: Policy) {
        self.policy = policy;
        let policy = &self.policy;
        self.tickets
            .retain(|(uid, id), _| policy.app(*uid, id).is_some());
        self.instances
            .retain(|(uid, id), _| policy.app(*uid, id).is_some());
        // An app the user stopped protecting must not keep its windows
        // hidden or its window permission.
        self.window_allowed.retain(|uid, apps| {
            apps.retain(|app| policy.app(*uid, app).is_some());
            !apps.is_empty()
        });
    }

    /// Name and wait limit of a user that has a policy.
    pub fn prompt_timeout(&self, uid: u32) -> Option<Duration> {
        self.policy.users.get(&uid).map(|u| u.prompt_timeout)
    }

    pub fn is_protected(&self, uid: u32, app: &str) -> bool {
        self.policy.app(uid, app).is_some()
    }

    /// Apps whose windows are allowed to show. This is not the ticket set:
    /// a ticket expiring only means the next launch needs authentication,
    /// while an open window stays usable until a lock.
    pub fn window_allowed(&self, uid: u32) -> Vec<String> {
        self.window_allowed
            .get(&uid)
            .map(|apps| apps.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn is_window_allowed(&self, uid: u32, app: &str) -> bool {
        self.window_allowed
            .get(&uid)
            .is_some_and(|apps| apps.contains(app))
    }

    /// Let this app's windows show until the next lock.
    pub fn allow_window(&mut self, uid: u32, app: &str) {
        if self.is_protected(uid, app) {
            self.window_allowed
                .entry(uid)
                .or_default()
                .insert(app.to_string());
        }
    }

    /// What `ListApps` reports for one user.
    /// App id, ticket state, configured paths and the paths actually marked.
    pub fn list_apps(&self, uid: u32, now: Instant) -> Vec<AppRow> {
        let Some(user) = self.policy.users.get(&uid) else {
            return Vec::new();
        };
        user.apps
            .iter()
            .map(|a| {
                let unlocked = self
                    .tickets
                    .get(&(uid, a.id.clone()))
                    .is_some_and(|t| t.expires > now);
                let paths = a.paths.iter().map(|p| p.display().to_string()).collect();
                let effective = a
                    .effective
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect();
                (a.id.clone(), unlocked, paths, effective)
            })
            .collect()
    }

    /// Grace period, autostart flag, protection state and unfocus relock
    /// delay for the settings UI.
    pub fn app_config(&self, uid: u32, app: &str) -> (u64, bool, bool, u64) {
        // A user without a config file still gets the defaults the first
        // authentication runs on, otherwise the settings window would claim a
        // grace period of zero.
        let Some(user) = self.policy.users.get(&uid) else {
            return (DEFAULT_GRACE.as_secs(), false, false, 0);
        };
        match user.apps.iter().find(|a| a.id == app) {
            Some(entry) => (
                entry.grace.as_secs(),
                entry.autostart,
                true,
                entry.relock.as_secs(),
            ),
            None => (user.global_grace.as_secs(), false, false, 0),
        }
    }

    /// Per-app unfocus relock delays the window layer has to track. Apps
    /// with the feature off are left out.
    pub fn relock_seconds(&self, uid: u32) -> Vec<(String, u64)> {
        let Some(user) = self.policy.users.get(&uid) else {
            return Vec::new();
        };
        user.apps
            .iter()
            .filter(|a| !a.relock.is_zero())
            .map(|a| (a.id.clone(), a.relock.as_secs()))
            .collect()
    }

    /// The user's global settings for the settings window: grace period,
    /// whether the screen lock drops tickets, and the wait limit. A user
    /// without a config file gets the defaults the first authentication runs
    /// on.
    pub fn global_config(&self, uid: u32) -> (u64, bool, u64) {
        let Some(user) = self.policy.users.get(&uid) else {
            return (
                DEFAULT_GRACE.as_secs(),
                true,
                DEFAULT_PROMPT_TIMEOUT.as_secs(),
            );
        };
        (
            user.global_grace.as_secs(),
            user.clear_on_lock,
            user.prompt_timeout.as_secs(),
        )
    }

    pub fn set_session_start(&mut self, uid: u32, start: Instant) {
        self.session_start.insert(uid, start);
    }

    pub fn session_start(&self, uid: u32) -> Option<Instant> {
        self.session_start.get(&uid).copied()
    }

    /// Drop one app's ticket without touching its window permission. A
    /// changed grace period applies to the next launch, and must not hide a
    /// window that is still in use (docs/subsystems/configuration.md).
    pub fn drop_ticket(&mut self, uid: u32, app: &str) -> bool {
        self.tickets.remove(&(uid, app.to_string())).is_some()
    }

    /// Drop one app's ticket and window permission. Returns the app id when
    /// either was there.
    pub fn lock_app(&mut self, uid: u32, app: &str) -> Option<String> {
        let ticket = self.tickets.remove(&(uid, app.to_string())).is_some();
        let window = self
            .window_allowed
            .get_mut(&uid)
            .is_some_and(|apps| apps.remove(app));
        if self
            .window_allowed
            .get(&uid)
            .is_some_and(|apps| apps.is_empty())
        {
            self.window_allowed.remove(&uid);
        }
        (ticket || window).then(|| app.to_string())
    }

    /// The screen of this user locked. Drops the tickets unless the user
    /// turned `clear_on_screen_lock` off. Returns the apps that had one.
    pub fn screen_locked(&mut self, uid: u32) -> Vec<String> {
        if self
            .policy
            .users
            .get(&uid)
            .is_some_and(|u| !u.clear_on_lock)
        {
            return Vec::new();
        }
        self.clear_tickets(uid)
    }

    /// Drop every ticket and window permission of a user. Returns the apps
    /// that had either.
    pub fn clear_tickets(&mut self, uid: u32) -> Vec<String> {
        let mut cleared = Vec::new();
        self.tickets.retain(|(u, app), _| {
            if *u == uid {
                cleared.push(app.clone());
                false
            } else {
                true
            }
        });
        // A window can be allowed after its ticket was used up. It still has
        // to disappear when the user locks.
        for app in self.window_allowed.remove(&uid).unwrap_or_default() {
            if !cleared.contains(&app) {
                cleared.push(app);
            }
        }
        cleared
    }

    /// Answer an exec request for `file` made by process `pid` (docs/subsystems/launch-gate.md).
    pub fn decide(&mut self, procs: &impl Procs, file: FileId, pid: i32, now: Instant) -> Decision {
        if !self.policy.covers(file) {
            return Decision::allow(Why::Unprotected);
        }
        let Some(uid) = procs.uid(pid) else {
            return Decision::allow(Why::CallerGone);
        };
        // Several apps of one user may share a protected path (docs/subsystems/launch-gate.md).
        // They cannot be told apart at exec time, so any of them may allow.
        let apps: Vec<(String, bool)> = match self.policy.users.get(&uid) {
            Some(user) => user
                .apps
                .iter()
                .filter(|a| a.files.contains(&file))
                .map(|a| (a.id.clone(), a.autostart))
                .collect(),
            None => Vec::new(),
        };
        if apps.is_empty() {
            return Decision::allow(Why::OtherUser);
        }
        if let Some(exe) = procs.exe(pid)
            && let Some((id, _)) = apps
                .iter()
                .find(|(id, _)| self.files_of(uid, id).contains(&exe))
        {
            return Decision::allow_app(uid, id, Why::SelfExec);
        }
        let chain = ancestors(procs, pid);
        for (id, _) in &apps {
            let key = (uid, id.clone());
            if let Some(list) = self.instances.get_mut(&key) {
                list.retain(|i| alive(procs, i));
                if list.iter().any(|i| chain.contains(i)) {
                    return Decision::allow_app(uid, id, Why::Instance);
                }
            }
        }
        let mut lost = Vec::new();
        for (id, _) in &apps {
            let key = (uid, id.clone());
            match self.take_ticket(&key, now) {
                TicketUse::Valid { one_shot } => {
                    self.remember_instance(key, chain.first());
                    let mut decision = Decision::allow_app(uid, id, Why::Ticket);
                    if one_shot {
                        lost.push(id.clone());
                    }
                    decision.lost_tickets = lost;
                    return decision;
                }
                TicketUse::Expired => lost.push(id.clone()),
                TicketUse::None => {}
            }
        }
        let mut decision = if let Some(id) = self.autostart_app(&uid, &apps, procs, pid, now) {
            let key = (uid, id.to_string());
            self.remember_instance(key, chain.first());
            // The policy lets this launch through without a box, so its
            // window may show too.
            self.allow_window(uid, id);
            Decision::allow_app(uid, id, Why::Autostart)
        } else {
            Decision {
                why: Why::NeedAuth,
                auth: Some((uid, apps[0].0.clone())),
                uid: Some(uid),
                app: Some(apps[0].0.clone()),
                lost_tickets: Vec::new(),
            }
        };
        decision.lost_tickets = lost;
        decision
    }

    fn files_of(&self, uid: u32, id: &str) -> Vec<FileId> {
        self.policy
            .app(uid, id)
            .map(|a| a.files.clone())
            .unwrap_or_default()
    }

    /// The app whose autostart unit launched this process, if any. See docs/subsystems/launch-gate.md.
    /// The unit name can be forged by the same user, which the settings
    /// UI has to say. Several apps can share a protected path, so the unit
    /// name, not the first app in the list, decides which instance is
    /// registered.
    fn autostart_app<'a>(
        &self,
        uid: &u32,
        apps: &'a [(String, bool)],
        procs: &impl Procs,
        pid: i32,
        now: Instant,
    ) -> Option<&'a str> {
        if !self.policy.users.contains_key(uid) {
            return None;
        }
        let start = self.session_start.get(uid)?;
        if now.saturating_duration_since(*start) >= AUTOSTART_WINDOW {
            return None;
        }
        let cgroup = procs.cgroup(pid)?;
        apps.iter()
            .find(|(id, autostart)| {
                *autostart
                    && cgroup.ends_with(&format!("/app-{}@autostart.service", unit_name_escape(id)))
            })
            .map(|(id, _)| id.as_str())
    }

    fn remember_instance(&mut self, key: Key, me: Option<&ProcId>) {
        if let Some(me) = me {
            let list = self.instances.entry(key).or_default();
            if !list.contains(me) {
                list.push(*me);
            }
        }
    }

    /// Register the process that was just allowed to start as an instance.
    /// Its descendants count as the app while it is alive (docs/subsystems/launch-gate.md).
    pub fn register_instance(&mut self, uid: u32, app: &str, pid: i32, procs: &impl Procs) {
        if let Some((_, start)) = procs.stat(pid) {
            let me = ProcId { pid, start };
            self.remember_instance((uid, app.to_string()), Some(&me));
        }
    }

    fn take_ticket(&mut self, key: &Key, now: Instant) -> TicketUse {
        match self.tickets.get(key) {
            Some(t) if t.expires > now => {
                let one_shot = t.one_shot;
                if one_shot {
                    self.tickets.remove(key);
                }
                TicketUse::Valid { one_shot }
            }
            Some(_) => {
                self.tickets.remove(key);
                TicketUse::Expired
            }
            None => TicketUse::None,
        }
    }

    /// Issue a ticket after a successful authentication. The grace period
    /// counts from now (docs/subsystems/launch-gate.md). Only an app the user protects has a
    /// ticket: a policy reload between the request and the answer drops it
    /// here instead of leaving one for an app that is no longer listed.
    pub fn grant(&mut self, uid: u32, app: &str, now: Instant) -> Option<Duration> {
        self.grant_with(uid, app, now, GrantUse::ForLater)
    }

    /// A held exec was authenticated. For a zero grace period no ticket is
    /// stored: the event loop allows the waiting exec itself, and a one-shot
    /// ticket left in the table would let a second launch slip through
    /// (docs/subsystems/launch-gate.md).
    pub fn grant_for_held_exec(&mut self, uid: u32, app: &str, now: Instant) -> Option<Duration> {
        self.grant_with(uid, app, now, GrantUse::HeldExec)
    }

    fn grant_with(
        &mut self,
        uid: u32,
        app: &str,
        now: Instant,
        use_case: GrantUse,
    ) -> Option<Duration> {
        let (grace, wait) = {
            let user = self.policy.users.get(&uid)?;
            let entry = user.apps.iter().find(|a| a.id == app)?;
            (entry.grace, user.prompt_timeout)
        };
        let ticket = if grace.is_zero() {
            match use_case {
                // `shisa unlock` has nothing waiting, so the ticket stays
                // valid for one wait limit and is consumed by the first
                // launch.
                GrantUse::ForLater => Some(Ticket {
                    expires: now + wait,
                    one_shot: true,
                }),
                // The exec that waited for this authentication is allowed by
                // the event loop. A second launch must ask again.
                GrantUse::HeldExec => None,
            }
        } else {
            Some(Ticket {
                expires: now + grace,
                one_shot: false,
            })
        };
        if let Some(ticket) = ticket {
            self.tickets.insert((uid, app.to_string()), ticket);
        }
        // The authentication was for this app, so its windows may show even
        // after a zero-grace ticket is used up.
        self.allow_window(uid, app);
        Some(grace)
    }

    pub fn has_ticket(&self, uid: u32, app: &str, now: Instant) -> bool {
        self.tickets
            .get(&(uid, app.to_string()))
            .is_some_and(|t| t.expires > now)
    }

    /// Drop expired tickets and instances whose process has exited. Returns
    /// the apps whose ticket expired, so the event loop can tell their
    /// session agent.
    pub fn prune(&mut self, procs: &impl Procs, now: Instant) -> Vec<(u32, String)> {
        let mut expired = Vec::new();
        self.tickets.retain(|(uid, app), t| {
            if t.expires > now {
                true
            } else {
                expired.push((*uid, app.clone()));
                false
            }
        });
        self.instances.retain(|_, list| {
            list.retain(|i| alive(procs, i));
            !list.is_empty()
        });
        expired
    }
}

/// systemd's unit name escaping, which is how a desktop id appears inside
/// `app-<id>@autostart.service`: `firefox-nightly` becomes
/// `firefox\x2dnightly`. Everything but ASCII alphanumerics, ':', '_' and '.'
/// is written as `\xXX`, and so is a leading '.'.
fn unit_name_escape(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for (i, b) in id.bytes().enumerate() {
        let plain = b.is_ascii_alphanumeric() || matches!(b, b':' | b'_') || (b == b'.' && i > 0);
        if plain {
            out.push(b as char);
        } else if b == b'/' {
            out.push('-');
        } else {
            out.push_str(&format!("\\x{b:02x}"));
        }
    }
    out
}

fn alive(procs: &impl Procs, id: &ProcId) -> bool {
    procs
        .stat(id.pid)
        .is_some_and(|(_, start)| start == id.start)
}

/// The process itself followed by its ancestors, nearest first.
fn ancestors(procs: &impl Procs, pid: i32) -> Vec<ProcId> {
    let mut chain = Vec::new();
    let mut cur = pid;
    while cur > 1 && chain.len() < MAX_ANCESTORS {
        let Some((ppid, start)) = procs.stat(cur) else {
            break;
        };
        chain.push(ProcId { pid: cur, start });
        cur = ppid;
    }
    chain
}

#[cfg(test)]
mod tests {
    use super::*;

    const UID: u32 = 1000;
    const APP_BIN: FileId = FileId { dev: 1, ino: 10 };
    const APP_ALT: FileId = FileId { dev: 1, ino: 11 };
    const SHARED: FileId = FileId { dev: 1, ino: 20 };
    const SHELL: FileId = FileId { dev: 1, ino: 99 };

    struct Proc {
        uid: u32,
        exe: FileId,
        ppid: i32,
        start: u64,
        cgroup: String,
    }

    #[derive(Default)]
    struct Fake(HashMap<i32, Proc>);

    impl Fake {
        fn add(&mut self, pid: i32, ppid: i32, uid: u32, exe: FileId) {
            let cgroup = format!("0::/user.slice/user-{uid}.slice/session-1.scope");
            self.0.insert(
                pid,
                Proc {
                    uid,
                    exe,
                    ppid,
                    start: pid as u64 * 7,
                    cgroup,
                },
            );
        }
    }

    impl Procs for Fake {
        fn uid(&self, pid: i32) -> Option<u32> {
            self.0.get(&pid).map(|p| p.uid)
        }
        fn exe(&self, pid: i32) -> Option<FileId> {
            self.0.get(&pid).map(|p| p.exe)
        }
        fn stat(&self, pid: i32) -> Option<(i32, u64)> {
            self.0.get(&pid).map(|p| (p.ppid, p.start))
        }
        fn cgroup(&self, pid: i32) -> Option<String> {
            self.0.get(&pid).map(|p| p.cgroup.clone())
        }
    }

    fn app(id: &str, grace: u64, files: &[FileId]) -> AppPolicy {
        AppPolicy {
            id: id.into(),
            grace: Duration::from_secs(grace),
            autostart: false,
            relock: Duration::ZERO,
            paths: Vec::new(),
            effective: Vec::new(),
            files: files.to_vec(),
        }
    }

    fn state() -> State {
        let mut autostart = app("auto", 300, &[FileId { dev: 1, ino: 40 }]);
        autostart.autostart = true;
        let user = UserPolicy {
            name: "tester".into(),
            prompt_timeout: Duration::from_secs(60),
            global_grace: Duration::from_secs(120),
            clear_on_lock: true,
            apps: vec![
                app("app", 300, &[APP_BIN, APP_ALT]),
                app("once", 0, &[FileId { dev: 1, ino: 30 }]),
                app("twin-a", 300, &[SHARED]),
                app("twin-b", 300, &[SHARED]),
                autostart,
            ],
        };
        let mut s = State::default();
        s.set_policy(Policy {
            users: HashMap::from([(UID, user)]),
        });
        s
    }

    /// pid 100 is the user's shell, pid 200 a child of it about to exec.
    fn procs() -> Fake {
        let mut p = Fake::default();
        p.add(100, 1, UID, SHELL);
        p.add(200, 100, UID, SHELL);
        p
    }

    #[test]
    fn unprotected_file_is_allowed() {
        let mut s = state();
        let d = s.decide(&procs(), FileId { dev: 9, ino: 9 }, 200, Instant::now());
        assert_eq!(d.why, Why::Unprotected);
        assert!(d.why.allows());
        assert_eq!(d.auth, None);
    }

    #[test]
    fn other_users_are_not_affected() {
        let mut s = state();
        let mut p = procs();
        p.add(300, 1, 0, SHELL);
        p.add(301, 1, 1001, SHELL);
        assert_eq!(
            s.decide(&p, APP_BIN, 300, Instant::now()).why,
            Why::OtherUser
        );
        assert_eq!(
            s.decide(&p, APP_BIN, 301, Instant::now()).why,
            Why::OtherUser
        );
    }

    #[test]
    fn denied_without_ticket_and_the_app_is_named() {
        let mut s = state();
        let d = s.decide(&procs(), APP_BIN, 200, Instant::now());
        assert_eq!(d.why, Why::NeedAuth);
        assert!(!d.why.allows());
        assert_eq!(d.auth, Some((UID, "app".to_string())));
    }

    #[test]
    fn vanished_caller_is_denied() {
        let mut s = state();
        let d = s.decide(&procs(), APP_BIN, 555, Instant::now());
        assert_eq!(d.why, Why::CallerGone);
        assert!(!d.why.allows());
        assert_eq!(d.auth, None);
    }

    #[test]
    fn ticket_allows_until_grace_ends() {
        let mut s = state();
        let p = procs();
        let t0 = Instant::now();
        assert_eq!(s.grant(UID, "app", t0), Some(Duration::from_secs(300)));
        assert!(s.has_ticket(UID, "app", t0));
        let first = s.decide(&p, APP_BIN, 200, t0 + Duration::from_secs(299));
        assert_eq!(first.why, Why::Ticket);
        assert!(first.lost_tickets.is_empty());
        let mut p = p;
        p.add(400, 100, UID, SHELL);
        let expired = s.decide(&p, APP_ALT, 400, t0 + Duration::from_secs(300));
        assert_eq!(expired.why, Why::NeedAuth);
        assert_eq!(expired.lost_tickets, vec!["app".to_string()]);
        assert!(!s.has_ticket(UID, "app", t0 + Duration::from_secs(300)));
    }

    #[test]
    fn ticket_is_per_app_and_per_user() {
        let mut s = state();
        let t0 = Instant::now();
        s.grant(UID, "app", t0);
        assert_eq!(s.decide(&procs(), SHARED, 200, t0).why, Why::NeedAuth);
        // No policy for the account, or no such app in it: there is nothing
        // an authentication could leave a ticket for.
        assert_eq!(s.grant(1001, "app", t0), None);
        assert_eq!(s.grant(UID, "nope", t0), None);
    }

    #[test]
    fn zero_grace_ticket_covers_one_launch() {
        let mut s = state();
        let mut p = procs();
        p.add(201, 100, UID, SHELL);
        let once = FileId { dev: 1, ino: 30 };
        let t0 = Instant::now();
        assert_eq!(s.grant(UID, "once", t0), Some(Duration::ZERO));
        let first = s.decide(&p, once, 200, t0);
        assert_eq!(first.why, Why::Ticket);
        assert_eq!(first.lost_tickets, vec!["once".to_string()]);
        let second = s.decide(&p, once, 201, t0);
        assert_eq!(second.why, Why::NeedAuth);
        assert!(second.lost_tickets.is_empty());
        s.grant(UID, "once", t0);
        let late = s.decide(&p, once, 201, t0 + Duration::from_secs(60));
        assert_eq!(late.why, Why::NeedAuth);
        assert_eq!(late.lost_tickets, vec!["once".to_string()]);
    }

    #[test]
    fn a_used_ticket_does_not_hide_an_open_window() {
        let mut s = state();
        let mut p = procs();
        p.add(201, 100, UID, SHELL);
        let once = FileId { dev: 1, ino: 30 };
        let t0 = Instant::now();
        assert_eq!(s.grant(UID, "once", t0), Some(Duration::ZERO));
        assert!(s.is_window_allowed(UID, "once"));
        let first = s.decide(&p, once, 201, t0);
        assert_eq!(first.why, Why::Ticket);
        assert!(!s.has_ticket(UID, "once", t0));
        // The launch used up the ticket. The window is still in use, so it
        // stays visible through the grace period and the ticket sweep.
        assert!(s.is_window_allowed(UID, "once"));
        s.prune(&p, t0 + Duration::from_secs(3600));
        assert!(s.is_window_allowed(UID, "once"));
    }

    #[test]
    fn a_lock_drops_a_window_permission_without_a_ticket() {
        let mut s = state();
        s.allow_window(UID, "app");
        assert!(s.lock_app(UID, "app").is_some());
        assert!(!s.is_window_allowed(UID, "app"));
        assert!(s.lock_app(UID, "app").is_none());
    }

    #[test]
    fn clearing_tickets_reports_apps_whose_window_was_allowed() {
        let mut s = state();
        s.allow_window(UID, "app");
        assert_eq!(s.clear_tickets(UID), vec!["app".to_string()]);
        assert!(!s.is_window_allowed(UID, "app"));
    }

    #[test]
    fn the_screen_lock_hides_a_window_even_without_a_ticket() {
        let mut s = state();
        s.allow_window(UID, "app");
        assert_eq!(s.screen_locked(UID), vec!["app".to_string()]);
        assert!(!s.is_window_allowed(UID, "app"));
    }

    #[test]
    fn unprotecting_an_app_drops_its_window_permission() {
        let mut s = state();
        s.allow_window(UID, "app");
        let mut policy = s.policy().clone();
        policy
            .users
            .get_mut(&UID)
            .unwrap()
            .apps
            .retain(|a| a.id != "app");
        s.set_policy(policy);
        assert!(!s.is_window_allowed(UID, "app"));
    }

    #[test]
    fn prune_reports_expired_tickets() {
        let mut s = state();
        let p = procs();
        let t0 = Instant::now();
        s.grant(UID, "app", t0);
        s.grant(UID, "twin-a", t0);
        let expired = s.prune(&p, t0 + Duration::from_secs(301));
        let mut apps: Vec<&str> = expired
            .iter()
            .map(|(uid, app)| {
                assert_eq!(*uid, UID);
                app.as_str()
            })
            .collect();
        apps.sort();
        assert_eq!(apps, vec!["app", "twin-a"]);
        assert!(s.tickets.is_empty());
    }

    #[test]
    fn running_app_may_exec_itself_without_ticket() {
        let mut s = state();
        let mut p = procs();
        p.add(500, 1, UID, APP_BIN);
        assert_eq!(
            s.decide(&p, APP_ALT, 500, Instant::now()).why,
            Why::SelfExec
        );
        assert_eq!(s.decide(&p, SHARED, 500, Instant::now()).why, Why::NeedAuth);
    }

    #[test]
    fn descendants_of_an_instance_are_allowed_after_ticket_expiry() {
        let mut s = state();
        let mut p = procs();
        let t0 = Instant::now();
        s.grant(UID, "app", t0);
        assert_eq!(s.decide(&p, APP_BIN, 200, t0).why, Why::Ticket);
        p.add(210, 200, UID, SHELL);
        p.add(211, 210, UID, SHELL);
        let late = t0 + Duration::from_secs(900);
        assert_eq!(s.decide(&p, APP_BIN, 211, late).why, Why::Instance);
        p.add(220, 100, UID, SHELL);
        assert_eq!(s.decide(&p, APP_BIN, 220, late).why, Why::NeedAuth);
    }

    #[test]
    fn instance_ends_with_its_process_and_pid_reuse_does_not_revive_it() {
        let mut s = state();
        let mut p = procs();
        let t0 = Instant::now();
        s.grant(UID, "app", t0);
        assert_eq!(s.decide(&p, APP_BIN, 200, t0).why, Why::Ticket);
        let late = t0 + Duration::from_secs(900);
        p.0.insert(
            200,
            Proc {
                uid: UID,
                exe: SHELL,
                ppid: 100,
                start: 123456,
                cgroup: String::new(),
            },
        );
        p.add(230, 200, UID, SHELL);
        assert_eq!(s.decide(&p, APP_BIN, 230, late).why, Why::NeedAuth);
        s.prune(&p, late);
        assert!(s.instances.is_empty());
        assert!(s.tickets.is_empty());
    }

    #[test]
    fn apps_sharing_a_path_accept_either_ticket() {
        let mut s = state();
        let t0 = Instant::now();
        s.grant(UID, "twin-b", t0);
        assert_eq!(s.decide(&procs(), SHARED, 200, t0).why, Why::Ticket);
        assert!(s.instances.contains_key(&(UID, "twin-b".to_string())));
    }

    #[test]
    fn reload_drops_state_of_removed_apps() {
        let mut s = state();
        let p = procs();
        let t0 = Instant::now();
        s.grant(UID, "app", t0);
        s.decide(&p, APP_BIN, 200, t0);
        s.grant(UID, "twin-a", t0);
        let mut policy = s.policy().clone();
        policy
            .users
            .get_mut(&UID)
            .unwrap()
            .apps
            .retain(|a| a.id == "twin-a");
        s.set_policy(policy);
        assert!(!s.is_protected(UID, "app"));
        assert_eq!(s.decide(&p, APP_BIN, 200, t0).why, Why::Unprotected);
        assert_eq!(s.tickets.len(), 1);
        assert!(s.instances.is_empty());
    }

    #[test]
    fn locking_drops_that_users_tickets_only() {
        let mut s = state();
        let t0 = Instant::now();
        s.grant(UID, "app", t0);
        s.grant(UID, "twin-a", t0);
        let mut cleared = s.clear_tickets(UID);
        cleared.sort();
        assert_eq!(cleared, vec!["app".to_string(), "twin-a".to_string()]);
        assert!(!s.has_ticket(UID, "app", t0));
        assert!(s.clear_tickets(1001).is_empty());
        s.grant(UID, "app", t0);
        assert_eq!(s.lock_app(UID, "app"), Some("app".into()));
        assert_eq!(s.lock_app(UID, "app"), None);
    }

    #[test]
    fn list_apps_reports_tickets_and_paths() {
        let mut s = state();
        let t0 = Instant::now();
        let mut policy = s.policy().clone();
        let app = &mut policy.users.get_mut(&UID).unwrap().apps[0];
        app.paths = vec!["/usr/bin/app".into(), "/opt/missing".into()];
        app.effective = vec!["/usr/bin/app".into()];
        s.set_policy(policy);
        s.grant(UID, "app", t0);
        let apps = s.list_apps(UID, t0);
        assert_eq!(
            apps[0],
            (
                "app".to_string(),
                true,
                vec!["/usr/bin/app".to_string(), "/opt/missing".to_string()],
                vec!["/usr/bin/app".to_string()]
            )
        );
        assert_eq!(apps[1], ("once".to_string(), false, vec![], vec![]));
        assert!(s.list_apps(1001, t0).is_empty());
        assert_eq!(s.app_config(UID, "app"), (300, false, true, 0));
        assert_eq!(s.app_config(UID, "once"), (0, false, true, 0));
        assert_eq!(s.app_config(UID, "absent"), (120, false, false, 0));
        assert_eq!(
            s.app_config(1001, "app"),
            (DEFAULT_GRACE.as_secs(), false, false, 0)
        );
        assert!(s.relock_seconds(UID).is_empty());
    }

    #[test]
    fn a_grace_change_drops_the_ticket_but_keeps_the_window() {
        let mut s = state();
        let t0 = Instant::now();
        s.grant(UID, "app", t0);
        assert!(s.has_ticket(UID, "app", t0));
        assert!(s.drop_ticket(UID, "app"));
        assert!(!s.has_ticket(UID, "app", t0));
        assert!(s.is_window_allowed(UID, "app"));
    }

    #[test]
    fn relock_seconds_lists_only_apps_with_the_feature_on() {
        let mut s = state();
        let mut policy = s.policy().clone();
        policy.users.get_mut(&UID).unwrap().apps[0].relock = Duration::from_secs(300);
        s.set_policy(policy);
        assert_eq!(s.relock_seconds(UID), vec![("app".to_string(), 300)]);
        assert_eq!(s.app_config(UID, "app").3, 300);
        assert!(s.relock_seconds(1001).is_empty());
    }

    #[test]
    fn global_config_reports_the_users_settings_or_the_defaults() {
        let mut s = state();
        assert_eq!(s.global_config(UID), (120, true, 60));
        s.policy.users.get_mut(&UID).unwrap().clear_on_lock = false;
        assert_eq!(s.global_config(UID), (120, false, 60));
        assert_eq!(
            s.global_config(1001),
            (
                DEFAULT_GRACE.as_secs(),
                true,
                DEFAULT_PROMPT_TIMEOUT.as_secs()
            )
        );
    }

    #[test]
    fn autostart_window_allows_only_the_matching_unit() {
        let auto = FileId { dev: 1, ino: 40 };
        let mut s = state();
        let mut p = procs();
        p.add(600, 1, UID, SHELL);
        p.0.get_mut(&600).unwrap().cgroup =
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-auto@autostart.service"
                .into();
        let t0 = Instant::now();
        s.set_session_start(UID, t0);
        assert_eq!(
            s.decide(&p, auto, 600, t0 + Duration::from_secs(5)).why,
            Why::Autostart
        );
        // The instance registered above keeps it working later without a ticket.
        assert_eq!(
            s.decide(&p, auto, 600, t0 + Duration::from_secs(60)).why,
            Why::Instance
        );

        let mut s = state();
        s.set_session_start(UID, t0);
        assert_eq!(
            s.decide(&p, auto, 600, t0 + Duration::from_secs(31)).why,
            Why::NeedAuth
        );
        assert_eq!(
            s.decide(&p, auto, 600, t0 + Duration::from_secs(5)).why,
            Why::Autostart
        );

        // Only the app's own autostart unit counts.
        let mut s = state();
        s.set_session_start(UID, t0);
        p.0.get_mut(&600).unwrap().cgroup =
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-other@autostart.service".into();
        assert_eq!(
            s.decide(&p, auto, 600, t0 + Duration::from_secs(5)).why,
            Why::NeedAuth
        );

        let mut s = state();
        assert_eq!(s.decide(&p, auto, 600, t0).why, Why::NeedAuth);
    }

    #[test]
    fn autostart_unit_names_are_escaped_like_systemd_does() {
        // Seen on a Plasma 6 session: app-kup\x2ddaemon@autostart.service.
        assert_eq!(unit_name_escape("kup-daemon"), "kup\\x2ddaemon");
        assert_eq!(unit_name_escape("org.fcitx.Fcitx5"), "org.fcitx.Fcitx5");
        assert_eq!(unit_name_escape(".hidden app"), "\\x2ehidden\\x20app");

        let file = FileId { dev: 1, ino: 41 };
        let mut dashed = app("fire-fox", 300, &[file]);
        dashed.autostart = true;
        let mut s = state();
        let mut policy = s.policy().clone();
        policy.users.get_mut(&UID).unwrap().apps.push(dashed);
        s.set_policy(policy);
        let mut p = procs();
        p.add(600, 1, UID, SHELL);
        p.0.get_mut(&600).unwrap().cgroup =
            "/user.slice/user-1000.slice/user@1000.service/app.slice/app-fire\\x2dfox@autostart.service".into();
        let t0 = Instant::now();
        s.set_session_start(UID, t0);
        assert_eq!(
            s.decide(&p, file, 600, t0 + Duration::from_secs(5)).why,
            Why::Autostart
        );
    }

    #[test]
    fn autostart_registers_the_app_whose_unit_matched() {
        let a_only = FileId { dev: 1, ino: 51 };
        let b_only = FileId { dev: 1, ino: 52 };
        // B is first and does not allow autostart. A shares the path and
        // does: the instance must belong to A, not to apps[0].
        let b = AppPolicy {
            id: "b".into(),
            grace: Duration::ZERO,
            autostart: false,
            relock: Duration::ZERO,
            paths: Vec::new(),
            effective: Vec::new(),
            files: vec![SHARED, b_only],
        };
        let a = AppPolicy {
            id: "a".into(),
            grace: Duration::ZERO,
            autostart: true,
            relock: Duration::ZERO,
            paths: Vec::new(),
            effective: Vec::new(),
            files: vec![SHARED, a_only],
        };
        let user = UserPolicy {
            name: "u".into(),
            prompt_timeout: Duration::from_secs(60),
            global_grace: Duration::ZERO,
            clear_on_lock: true,
            apps: vec![b, a],
        };
        let mut s = State::default();
        s.set_policy(Policy {
            users: HashMap::from([(UID, user)]),
        });
        let t0 = Instant::now();
        s.set_session_start(UID, t0);
        let mut p = procs();
        p.add(600, 1, UID, SHELL);
        p.0.get_mut(&600).unwrap().cgroup = "/x/app-a@autostart.service".into();
        assert_eq!(
            s.decide(&p, SHARED, 600, t0 + Duration::from_secs(5)).why,
            Why::Autostart
        );
        // A's own file is covered by A's instance after the window ends.
        assert_eq!(
            s.decide(&p, a_only, 600, t0 + Duration::from_secs(60)).why,
            Why::Instance
        );
        // B's file is not: no instance was registered for B.
        assert_eq!(
            s.decide(&p, b_only, 600, t0 + Duration::from_secs(60)).why,
            Why::NeedAuth
        );
    }

    #[test]
    fn a_held_exec_gets_no_one_shot_ticket() {
        let mut s = state();
        let now = Instant::now();
        // "once" has a zero grace period in the test policy.
        assert_eq!(
            s.grant_for_held_exec(UID, "once", now),
            Some(Duration::ZERO)
        );
        assert!(!s.has_ticket(UID, "once", now));
        // `shisa unlock` has no held exec, so its ticket is for the next launch.
        assert_eq!(s.grant(UID, "once", now), Some(Duration::ZERO));
        assert!(s.has_ticket(UID, "once", now));
    }

    #[test]
    fn register_instance_makes_descendants_count() {
        let mut s = state();
        let mut p = procs();
        p.add(400, 100, UID, SHELL);
        s.register_instance(UID, "app", 400, &p);
        p.add(500, 400, UID, SHELL);
        assert_eq!(
            s.decide(&p, APP_BIN, 500, Instant::now()).why,
            Why::Instance
        );
    }

    #[test]
    fn the_largest_grace_period_does_not_overflow() {
        let mut s = state();
        let mut policy = s.policy().clone();
        let user = policy.users.get_mut(&UID).unwrap();
        user.global_grace = Duration::from_secs(shisa_core::config::MAX_GRACE);
        user.apps[0].grace = Duration::from_secs(shisa_core::config::MAX_GRACE);
        s.set_policy(policy);
        let now = Instant::now();
        assert!(s.grant(UID, "app", now).is_some());
        assert!(s.has_ticket(UID, "app", now));
    }

    #[test]
    fn screen_lock_respects_the_user_setting() {
        let now = Instant::now();
        let mut s = state();
        s.grant(UID, "app", now);
        assert_eq!(s.screen_locked(UID), vec!["app".to_string()]);
        assert!(!s.has_ticket(UID, "app", now));

        let mut policy = s.policy().clone();
        policy.users.get_mut(&UID).unwrap().clear_on_lock = false;
        s.set_policy(policy);
        s.grant(UID, "app", now);
        assert!(s.screen_locked(UID).is_empty());
        assert!(s.has_ticket(UID, "app", now));
    }
}
