//! shisad: answers exec permission requests for protected applications.
//!
//! usage: shisad [--config-dir DIR] [--lifetime SECS] [--verbose]
//!
//! The main thread owns the fanotify descriptor and does nothing that can
//! block: it polls, decides from in-memory state plus a few /proc reads, and
//! answers. Authentication runs on other threads (see `polkit`). A request
//! that needs authentication is held until the polkit check succeeds or until
//! the wait limit passes. SIGHUP reloads the configuration on its own thread.
//! If this process dies the kernel allows every pending request and drops the
//! marks.
//!
//! Decisions are logged at debug level only (docs/subsystems/configuration.md).

use bus::{MAX_WAITING_AUTH, MAX_WAITING_AUTH_PER_USER, Msg};
use events::{Event, Events};
use gate::{Gate, Request};
use load::Marks;
use policy::{Change, Policy, State, Why};
use procfs::{Procs, RealProcs};
use shisa_core::dbus::{POLKIT_CHANGE, POLKIT_LAUNCH};
use shisad::{activity, bus, diag, events, gate, load, logind, notify, policy, polkit, procfs};
use std::collections::HashMap;
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
use std::{env, io, mem, process, thread};
use tracing::{debug, error, info, warn};

/// How often expired tickets and dead instances are swept.
const HOUSEKEEPING: Duration = Duration::from_secs(5);
/// How often the loop pings systemd's watchdog. The unit allows 10 seconds
/// between pings, and a busy loop would otherwise send one per event.
const WATCHDOG: Duration = Duration::from_secs(5);
/// How often the reload thread checks that every protected path still leads
/// to the inode that carries the mark. A package manager's hook covers
/// pacman, this covers everything else that replaces a file.
const REVALIDATE: Duration = Duration::from_secs(30);
/// A configuration that cannot be read or parsed is retried on the
/// revalidation tick. Repeated failures back off to this limit so a stale
/// file does not force a full mark pass every half minute.
const CONFIG_RETRY_MAX: Duration = Duration::from_secs(30 * 60);
/// Wait limit when the user's policy has none.
const DEFAULT_WAIT: Duration = policy::DEFAULT_PROMPT_TIMEOUT;
/// The most held execs one app and one user may have waiting at once. Each
/// waiting caller keeps a fanotify event descriptor in the daemon. A user can
/// start authenticated launches as fast as the kernel accepts them, and
/// answering the excess with a denial is better than running out of
/// descriptors and losing the whole gate.
const MAX_HELD_EXECS_PER_APP: usize = 64;
const MAX_HELD_EXECS_PER_USER: usize = 256;

struct Options {
    config_dir: PathBuf,
    lifetime: Option<Duration>,
    verbose: bool,
}

fn usage() -> ! {
    eprintln!("usage: shisad [--config-dir DIR] [--lifetime SECS] [--verbose]");
    process::exit(2);
}

fn options() -> Options {
    let mut o = Options {
        config_dir: shisa_core::CONFIG_DIR.into(),
        lifetime: None,
        verbose: false,
    };
    let mut args = env::args_os().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match arg.to_str() {
            Some("--config-dir") => o.config_dir = value().into(),
            Some("--lifetime") => {
                let secs = value()
                    .to_str()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| usage());
                o.lifetime = Some(Duration::from_secs(secs));
            }
            Some("--verbose") => o.verbose = true,
            _ => usage(),
        }
    }
    o
}

fn die(what: &str, e: impl std::fmt::Display) -> ! {
    error!("{what}: {e}");
    process::exit(1);
}

/// Route SIGHUP, SIGTERM and SIGINT to a descriptor the main loop polls.
/// Called before any thread starts so every thread inherits the mask.
fn signal_fd() -> io::Result<RawFd> {
    unsafe {
        let mut set: libc::sigset_t = mem::zeroed();
        libc::sigemptyset(&mut set);
        for sig in [libc::SIGHUP, libc::SIGTERM, libc::SIGINT] {
            libc::sigaddset(&mut set, sig);
        }
        if libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) != 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = libc::signalfd(-1, &set, libc::SFD_CLOEXEC | libc::SFD_NONBLOCK);
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(fd)
        }
    }
}

fn next_signal(fd: RawFd) -> Option<i32> {
    let mut info: libc::signalfd_siginfo = unsafe { mem::zeroed() };
    let n = unsafe { libc::read(fd, (&raw mut info).cast(), mem::size_of_val(&info)) };
    (n == mem::size_of_val(&info) as isize).then_some(info.ssi_signo as i32)
}

/// Work for the reload thread, which owns the marks.
enum Job {
    Reload,
    Change {
        id: u64,
        uid: u32,
        app: String,
        change: Change,
        /// The lock generations the request started under.
        revocation: (u64, u64),
        /// The settings authorization generation that must still be current.
        settings_revocation: u64,
    },
}

/// A settings change held until the user authenticates.
struct PendingChange {
    id: u64,
    uid: u32,
    app: String,
    change: Change,
    deadline: Instant,
    /// The lock generations this request started under.
    revocation: (u64, u64),
    /// The settings authorization generation this request started under.
    settings_revocation: u64,
}

/// An unlock request held until the user authenticates.
struct PendingUnlock {
    id: u64,
    uid: u32,
    deadline: Instant,
    /// The lock generations this request started under.
    revocation: (u64, u64),
}

/// Read, modify and write one user's config, then reload the policy. Runs on
/// the reload thread because it does file and inode work.
// The reload thread owns these pieces together. Bundling them only for this
// one caller would not make the state flow clearer.
#[allow(clippy::too_many_arguments)]
fn apply_change(
    gate: &Gate,
    dir: &Path,
    state: &Mutex<State>,
    commit: &Mutex<()>,
    marks: &mut Marks,
    diag: &diag::Diag,
    uid: u32,
    app: &str,
    change: &Change,
    revocation: (u64, u64),
    settings_revocation: u64,
) -> Result<(), String> {
    let name = shisa_core::users::name(uid).ok_or("unknown user")?;
    let mut config = load::read_user(dir, &name)?;
    load::apply(&mut config, app, change, |id| {
        // Only a new protection needs the desktop files. Reading the
        // session environment scans /proc, and an option change must not
        // fail just because the account has no home directory.
        let env = shisa_core::desktop::Env::for_user(uid).ok_or("no home directory")?;
        let found = shisa_core::desktop::resolve(id, &env).map_err(|e| e.to_string())?;
        if found.paths.is_empty() {
            let language = env.language();
            let notes: Vec<String> = found.notes.iter().map(|n| n.message(language)).collect();
            return Err(format!(
                "{id} has no usable protected path. {}",
                notes.join(" ")
            ));
        }
        Ok(found.paths)
    })?;
    // The write is the commit. A lock takes the same mutex before it clears
    // state, so a change that started before the lock either lands first or
    // is refused as a whole. The read and resolve above stay outside the
    // mutex because they can be slow and do not change what is protected.
    {
        let _commit = commit.lock().unwrap();
        {
            let state = state.lock().unwrap();
            if state.revocation(uid, app) != revocation
                || state.settings_revocation(uid) != settings_revocation
            {
                return Err("locked before the change was applied".to_string());
            }
        }
        load::write_user(dir, &name, &config).map_err(|e| e.to_string())?;
    }
    reload(gate, dir, state, marks, diag);
    Ok(())
}

/// Note in the diagnostics what the load found: a configured path that the
/// policy could not mark, and the time either way. A complete load clears the
/// earlier failure so the card answers for the current state.
fn note_marks(diag: &diag::Diag, policy: &Policy) {
    let complete = policy.users.values().all(|user| {
        user.apps.iter().all(|app| {
            // A listed app with no marked file stops nothing, which is a
            // failure whether its path list is empty or nothing in it could
            // be marked.
            !app.files.is_empty() && app.paths.iter().all(|path| app.effective.contains(path))
        })
    });
    if complete {
        diag.clear_failure();
    } else {
        diag.note_failure();
    }
    diag.note_reload();
}

fn reload(gate: &Gate, dir: &Path, state: &Mutex<State>, marks: &mut Marks, diag: &diag::Diag) {
    let old = state.lock().unwrap().policy().clone();
    // New marks are placed before the policy that refers to them is
    // installed. Events from them in between are allowed.
    let (new, config_failed) = load::load(gate, dir, &old, marks);
    if config_failed {
        diag.note_config_failure();
    } else {
        diag.clear_config_failure();
    }
    note_marks(diag, &new);
    let keep = new.files();
    state.lock().unwrap().set_policy(new);
    load::drop_stale_marks(gate, marks, &keep);
    info!(marks = marks.len(), "reload done");
}

/// An exec request held until the user authenticates.
struct Pending {
    id: u64,
    uid: u32,
    app: String,
    req: Request,
    deadline: Instant,
    /// The lock generations this request started under.
    revocation: (u64, u64),
}

impl Pending {
    fn allow(self) {
        debug!(pid = self.req.pid, app = self.app, "held exec allowed");
        let _ = self.req.answer(true);
    }

    fn deny(self) {
        debug!(pid = self.req.pid, app = self.app, "held exec denied");
        let _ = self.req.answer(false);
    }
}

fn on_request(
    state: &Mutex<State>,
    pending: &mut Vec<Pending>,
    events: &Arc<Events>,
    activity: &activity::Log,
    bus: &mpsc::Sender<Msg>,
    req: Request,
) {
    let pid = req.pid;
    let decision = match req.file() {
        Ok(file) => state
            .lock()
            .unwrap()
            .decide(&RealProcs, file, pid, Instant::now()),
        Err(e) => {
            // The event exists only for a marked inode. An unidentified file
            // cannot be proven unprotected, so deny it.
            warn!(pid, "cannot stat executed file: {e}, denying");
            let _ = req.answer(false);
            return;
        }
    };
    debug!(pid, why = ?decision.why, "exec request");
    // A one-shot ticket was used up, or an expired one was found while
    // answering. Either way the app is locked again, and the agent should
    // stop showing it as unlocked.
    if let Some(uid) = decision.uid {
        for app in &decision.lost_tickets {
            let _ = bus.send(Msg::StateChanged {
                uid,
                app: app.clone(),
                unlocked: false,
            });
        }
    }
    let Some((uid, app)) = decision.auth else {
        // A launch decided without the user. An unprotected file or another
        // user's app names no app of this caller, so nothing is recorded.
        if let (Some(uid), Some(app)) = (decision.uid, decision.app.as_deref()) {
            activity.record(uid, events.next_id(), app, pid, kind_of(decision.why), 0);
            // A ticket or autostart launch can restore the window permission
            // that focus-loss hiding removed. Tell the agent so its cached
            // window state catches up. A consumed one-shot ticket was already
            // reported above as lost.
            if matches!(decision.why, Why::Ticket | Why::Autostart)
                && !decision.lost_tickets.iter().any(|lost| lost == app)
            {
                let unlocked = state.lock().unwrap().has_ticket(uid, app, Instant::now());
                let _ = bus.send(Msg::StateChanged {
                    uid,
                    app: app.to_string(),
                    unlocked,
                });
            }
        }
        if let Err(e) = req.answer(decision.why.allows()) {
            debug!(pid, "answer not delivered: {e}");
        }
        return;
    };
    // The decision captured this while it held the policy lock, so a lock
    // that started before the prompt cannot be mistaken for one that
    // started after the exec request. A decision without it is a code bug:
    // denying keeps a lock that starts after the decision from being missed.
    let Some(revocation) = decision.revocation else {
        error!(
            pid,
            uid, %app, "authentication decision has no lock generation"
        );
        let _ = req.answer(false);
        return;
    };
    // A flood of launches must not grow the held set or the daemon's
    // descriptor table without bound. The caller is refused like any other
    // unauthenticated launch.
    if held_exec_crowded(pending.iter().map(|p| (p.uid, p.app.as_str())), uid, &app) {
        debug!(pid, uid, %app, "too many held launches, denying");
        activity.record(uid, events.next_id(), &app, pid, activity::DENIED, 0);
        let _ = req.answer(false);
        return;
    }
    // A second exec of an app that is already being asked about joins the
    // first one: one box, one answer for both. It also shares the first
    // request's lock generation, because the answer is committed once for
    // both.
    if let Some(first) = pending.iter().find(|p| p.uid == uid && p.app == app) {
        let (id, deadline, revocation) = (first.id, first.deadline, first.revocation);
        pending.push(Pending {
            id,
            uid,
            app,
            req,
            deadline,
            revocation,
        });
        return;
    }
    let timeout = state
        .lock()
        .unwrap()
        .prompt_timeout(uid)
        .unwrap_or(DEFAULT_WAIT);
    // polkit names this process as the subject. Without a start time the pid
    // could be reused before polkit looks at it.
    let Some(start) = RealProcs.stat(pid).map(|(_, start)| start) else {
        debug!(pid, "caller disappeared before authentication");
        activity.record(uid, events.next_id(), &app, pid, activity::DENIED, 0);
        let _ = req.answer(false);
        return;
    };
    let id = events.next_id();
    let deadline = Instant::now() + timeout;
    activity.record(
        uid,
        id,
        &app,
        pid,
        activity::ASKED,
        activity::after(timeout),
    );
    polkit::spawn(
        events.clone(),
        id,
        uid,
        app.clone(),
        POLKIT_LAUNCH,
        polkit::Subject::Process {
            pid: pid as u32,
            start,
        },
        deadline,
    );
    pending.push(Pending {
        id,
        uid,
        app,
        req,
        // polkit cancels at the deadline. This backup only matters if its
        // thread is stuck.
        deadline: deadline + Duration::from_secs(2),
        revocation,
    });
}

/// The activity kind of a launch that was decided without the user.
fn kind_of(why: Why) -> &'static str {
    match why {
        Why::Ticket => activity::TICKET,
        Why::Instance => activity::INSTANCE,
        Why::Autostart => activity::AUTOSTART,
        Why::SelfExec => activity::SELF_EXEC,
        _ => activity::DENIED,
    }
}

/// Take every element matching `pick` out of `list`, in one pass. Repeated
/// `Vec::remove` would be quadratic when many requests expire together.
fn take<T>(list: &mut Vec<T>, mut pick: impl FnMut(&T) -> bool) -> Vec<T> {
    let mut taken = Vec::new();
    let mut kept = Vec::with_capacity(list.len());
    for item in list.drain(..) {
        if pick(&item) {
            taken.push(item)
        } else {
            kept.push(item)
        }
    }
    *list = kept;
    taken
}

/// How an authentication request ended without a yes.
enum Ending {
    /// The wait limit passed, or the check answered after it.
    Cancelled,
    /// polkit answered that the user is not authorized.
    Denied,
    /// The check could not run or broke off.
    Failed(&'static str),
}

impl Ending {
    /// The activity record's kind.
    fn kind(&self) -> &'static str {
        match self {
            Ending::Cancelled => activity::CANCELLED,
            Ending::Denied => activity::DENIED,
            Ending::Failed(_) => activity::FAILED,
        }
    }

    /// What a waiting caller is told over the bus.
    fn message(&self) -> &str {
        match self {
            Ending::Cancelled => "authentication cancelled",
            Ending::Denied => "authentication was rejected",
            Ending::Failed(why) => why,
        }
    }
}

/// Everything that was waiting on `request` is refused: held execs are
/// denied, and bus callers get the reason.
fn refuse(
    pending: &mut Vec<Pending>,
    changes: &mut Vec<PendingChange>,
    unlocks: &mut Vec<PendingUnlock>,
    activity: &activity::Log,
    request: u64,
    ending: Ending,
    bus: &mpsc::Sender<Msg>,
) {
    for held in take(pending, |p| p.id == request) {
        debug!(
            request,
            pid = held.req.pid,
            kind = ending.kind(),
            "refusing a held exec"
        );
        activity.resolve(held.id, ending.kind());
        held.deny();
    }
    for change in take(changes, |c| c.id == request) {
        let _ = bus.send(Msg::ChangeDone {
            id: change.id,
            result: Err(ending.message().to_string()),
        });
    }
    for unlock in take(unlocks, |u| u.id == request) {
        let _ = bus.send(Msg::ChangeDone {
            id: unlock.id,
            result: Err(ending.message().to_string()),
        });
    }
}

/// Whether one more held exec of `app` for `uid` would pass either cap.
fn held_exec_crowded<'a>(held: impl Iterator<Item = (u32, &'a str)>, uid: u32, app: &str) -> bool {
    let (mut for_app, mut for_user) = (0, 0);
    for (held_uid, held_app) in held {
        if held_uid == uid {
            for_user += 1;
            if held_app == app {
                for_app += 1;
            }
        }
    }
    for_app >= MAX_HELD_EXECS_PER_APP || for_user >= MAX_HELD_EXECS_PER_USER
}

/// The bus enforces this budget before queueing a request. Rechecking the
/// accepted work here catches limits lowered while requests are in flight.
/// Settings changes and unlocks draw from a shared budget per user and overall.
fn auth_crowded(changes: &[PendingChange], unlocks: &[PendingUnlock], uid: u32) -> bool {
    let total = changes.len() + unlocks.len();
    let mine = changes.iter().filter(|c| c.uid == uid).count()
        + unlocks.iter().filter(|u| u.uid == uid).count();
    total >= MAX_WAITING_AUTH || mine >= MAX_WAITING_AUTH_PER_USER
}

/// Let a group of held execs through and remember each caller as an instance,
/// so its descendants count as the app while it is alive (docs/subsystems/launch-gate.md).
fn allow_held(
    state: &Mutex<State>,
    activity: &activity::Log,
    uid: u32,
    app: &str,
    held: Vec<Pending>,
) {
    if held.is_empty() {
        return;
    }
    for p in &held {
        activity.resolve(p.id, activity::GRANTED);
    }
    {
        let mut st = state.lock().unwrap();
        for p in &held {
            st.register_instance(uid, app, p.req.pid, &RealProcs);
        }
    }
    for p in held {
        p.allow();
    }
}

fn main() {
    let opts = options();
    let level = if opts.verbose {
        tracing::Level::DEBUG
    } else {
        tracing::Level::INFO
    };
    // stderr goes to the journal when run as a systemd unit.
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(io::stderr)
        .with_ansi(false)
        .without_time()
        .init();

    let signals = signal_fd().unwrap_or_else(|e| die("signalfd", e));
    let gate =
        Arc::new(Gate::new().unwrap_or_else(|e| die("fanotify_init (needs CAP_SYS_ADMIN)", e)));

    let mut marks = HashMap::new();
    // The window's diagnostics card reads this, never the fanotify loop.
    let diag = Arc::new(diag::Diag::new(opts.config_dir.clone()));
    let (policy, config_failed) =
        load::load(&gate, &opts.config_dir, &Default::default(), &mut marks);
    if config_failed {
        diag.note_config_failure();
    } else {
        diag.clear_config_failure();
    }
    note_marks(&diag, &policy);
    let state = Arc::new(Mutex::new(State::default()));
    state.lock().unwrap().set_policy(policy);
    let events = Arc::new(Events::new().unwrap_or_else(|e| die("event pipe", e)));

    // The bus is part of the daemon, not an optional extra: without it no
    // authentication can happen. Building it before anything else means a
    // second instance fails here and leaves the running one alone.
    // The settings window's activity page reads this, never the loop.
    let activity = Arc::new(activity::Log::default());
    // The commit lock serializes a lock with the write of an authenticated
    // settings change. Both sides take it before touching the state or the
    // config file, so a change either lands before the lock or not at all.
    let commit = Arc::new(Mutex::new(()));
    let bus_tx = bus::spawn(
        state.clone(),
        events.clone(),
        activity.clone(),
        diag.clone(),
        commit.clone(),
    )
    .unwrap_or_else(|e| die("system bus", e));
    if let Err(e) = logind::spawn(state.clone(), bus_tx.clone(), commit.clone()) {
        warn!("logind thread: {e}");
    }

    let notify = notify::Notify::connect();
    if notify.is_none() && env::var_os("NOTIFY_SOCKET").is_some() {
        warn!("NOTIFY_SOCKET is set but unusable, systemd will not receive READY");
    }

    let marked = marks.len();
    let (jobs, jobs_rx) = mpsc::channel::<Job>();
    {
        let (gate, state, dir) = (gate.clone(), state.clone(), opts.config_dir.clone());
        let bus = bus_tx.clone();
        let diag = diag.clone();
        let commit = commit.clone();
        let mut config_retry_at = Instant::now();
        let mut config_retry_delay = REVALIDATE;
        let worker = move || {
            loop {
                match jobs_rx.recv_timeout(REVALIDATE) {
                    Ok(Job::Reload) => {
                        info!("reloading configuration");
                        reload(&gate, &dir, &state, &mut marks, &diag);
                    }
                    Ok(Job::Change {
                        id,
                        uid,
                        app,
                        change,
                        revocation,
                        settings_revocation,
                    }) => {
                        info!(uid, %app, "applying a settings change");
                        let result = apply_change(
                            &gate,
                            &dir,
                            &state,
                            &commit,
                            &mut marks,
                            &diag,
                            uid,
                            &app,
                            &change,
                            revocation,
                            settings_revocation,
                        );
                        let applied = result.is_ok();
                        let _ = bus.send(Msg::ChangeDone { id, result });
                        // The window layer reads the per-app relock delay on
                        // refresh. Wake the agent so a setting change takes
                        // effect now instead of at the next 60 s refresh.
                        // A new grace period only applies to the next
                        // authentication, so an old ticket must not cover it.
                        if applied && !app.is_empty() {
                            if matches!(&change, Change::Grace(_)) {
                                state.lock().unwrap().drop_ticket(uid, &app);
                            }
                            let unlocked =
                                state.lock().unwrap().has_ticket(uid, &app, Instant::now());
                            let _ = bus.send(Msg::StateChanged { uid, app, unlocked });
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        // A failed configuration read or parse keeps its user
                        // out of the policy, so no mark can show the gap.
                        // Keep asking for a load until one succeeds instead
                        // of waiting for a signal after the file is repaired.
                        // Repeated failures back off so a stale file does not
                        // re-mark every protected path every half minute.
                        let config_failed = diag.config_failed() != 0;
                        if !config_failed {
                            config_retry_delay = REVALIDATE;
                            config_retry_at = Instant::now();
                        }
                        let retry_config = config_failed && Instant::now() >= config_retry_at;
                        let stale = {
                            let policy = state.lock().unwrap().policy().clone();
                            load::needs_reload(&policy, retry_config)
                        };
                        if !stale {
                            continue;
                        }
                        if retry_config {
                            info!("retrying a configuration that failed to load");
                        } else {
                            // Compare the files on disk with the inodes that
                            // actually carry marks, not with an earlier
                            // snapshot. A replacement during a reload and a
                            // mark that failed both show up here and are
                            // retried.
                            info!(
                                "a protected file changed or is not marked, placing the marks again"
                            );
                        }
                        reload(&gate, &dir, &state, &mut marks, &diag);
                        if diag.config_failed() != 0 {
                            config_retry_delay =
                                config_retry_delay.saturating_mul(2).min(CONFIG_RETRY_MAX);
                            config_retry_at = Instant::now() + config_retry_delay;
                        } else {
                            config_retry_delay = REVALIDATE;
                            config_retry_at = Instant::now();
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        };
        thread::Builder::new()
            .name("reload".into())
            .spawn(worker)
            .unwrap_or_else(|e| die("reload thread", e));
    }

    // Every thread the loop needs exists now, so systemd may treat the unit
    // as started.
    if let Some(notify) = &notify
        && !notify.ready()
    {
        warn!("systemd did not receive READY");
    }
    info!(
        marks = marked,
        users = state.lock().unwrap().policy().users.len(),
        "shisad ready"
    );

    let started = Instant::now();
    let mut swept = started;
    let mut pinged = started;
    let mut pending: Vec<Pending> = Vec::new();
    let mut changes: Vec<PendingChange> = Vec::new();
    let mut unlocks: Vec<PendingUnlock> = Vec::new();
    let mut fds = [
        libc::pollfd {
            fd: gate.fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: signals,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: events.fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    'main: loop {
        if pinged.elapsed() >= WATCHDOG {
            if let Some(notify) = &notify {
                notify.watchdog();
            }
            pinged = Instant::now();
        }
        let now = Instant::now();
        let fail = |id: u64, why: &str| {
            let _ = bus_tx.send(Msg::ChangeDone {
                id,
                result: Err(why.to_string()),
            });
        };
        for held in take(&mut pending, |p| p.deadline <= now) {
            activity.resolve(held.id, activity::TIMEOUT);
            held.deny();
        }
        for change in take(&mut changes, |c| c.deadline <= now) {
            fail(change.id, "authentication timed out");
        }
        for unlock in take(&mut unlocks, |u| u.deadline <= now) {
            fail(unlock.id, "authentication timed out");
        }
        for event in events.drain() {
            match event {
                Event::Cancelled { request } => {
                    refuse(
                        &mut pending,
                        &mut changes,
                        &mut unlocks,
                        &activity,
                        request,
                        Ending::Cancelled,
                        &bus_tx,
                    );
                }
                Event::Denied { request } => {
                    refuse(
                        &mut pending,
                        &mut changes,
                        &mut unlocks,
                        &activity,
                        request,
                        Ending::Denied,
                        &bus_tx,
                    );
                }
                Event::Failed { request, why } => {
                    // A polkit error closes the box, so nothing can answer
                    // this request any more. Refuse it now instead of leaving
                    // the caller held until the wait limit.
                    refuse(
                        &mut pending,
                        &mut changes,
                        &mut unlocks,
                        &activity,
                        request,
                        Ending::Failed(why),
                        &bus_tx,
                    );
                }
                Event::BusLost => {
                    error!("system bus connection closed, shutting down");
                    break 'main;
                }
                Event::ChangeRequest {
                    id,
                    uid,
                    app,
                    change,
                    sender,
                    revocation,
                    settings_revocation,
                } => {
                    // Every change is authenticated on its own. A ticket
                    // lets an app start, it does not let settings change.
                    if auth_crowded(&changes, &unlocks, uid) {
                        fail(id, "too many pending authentication requests");
                        continue;
                    }
                    let timeout = state
                        .lock()
                        .unwrap()
                        .prompt_timeout(uid)
                        .unwrap_or(DEFAULT_WAIT);
                    let deadline = Instant::now() + timeout;
                    polkit::spawn(
                        events.clone(),
                        id,
                        uid,
                        app.clone(),
                        POLKIT_CHANGE,
                        polkit::Subject::BusName(sender),
                        deadline,
                    );
                    changes.push(PendingChange {
                        id,
                        uid,
                        app,
                        change,
                        deadline: deadline + Duration::from_secs(2),
                        revocation,
                        settings_revocation,
                    });
                }
                Event::UnlockRequest {
                    id,
                    uid,
                    app,
                    sender,
                    revocation,
                } => {
                    if auth_crowded(&changes, &unlocks, uid) {
                        fail(id, "too many pending authentication requests");
                        continue;
                    }
                    let timeout = state
                        .lock()
                        .unwrap()
                        .prompt_timeout(uid)
                        .unwrap_or(DEFAULT_WAIT);
                    let deadline = Instant::now() + timeout;
                    polkit::spawn(
                        events.clone(),
                        id,
                        uid,
                        app,
                        POLKIT_LAUNCH,
                        polkit::Subject::BusName(sender),
                        deadline,
                    );
                    unlocks.push(PendingUnlock {
                        id,
                        uid,
                        deadline: deadline + Duration::from_secs(2),
                        revocation,
                    });
                }
                Event::Granted { uid, app, request } => {
                    // An answer to a settings request applies that change
                    // and nothing else.
                    let id = request;
                    {
                        let own = take(&mut changes, |c| c.id == id && c.uid == uid);
                        if !own.is_empty() {
                            for done in own {
                                let job = Job::Change {
                                    id: done.id,
                                    uid,
                                    app: done.app,
                                    change: done.change,
                                    revocation: done.revocation,
                                    settings_revocation: done.settings_revocation,
                                };
                                let _ = jobs.send(job);
                            }
                            continue;
                        }
                        // An unlock request leaves a ticket and nothing else.
                        let wanted = take(&mut unlocks, |u| u.id == id && u.uid == uid);
                        if !wanted.is_empty() {
                            // The ticket commits only when no lock started
                            // after the request. The state lock also holds
                            // the lock's own revocation, so the check and
                            // the grant cannot interleave with it.
                            let (revoked, granted) = {
                                let mut st = state.lock().unwrap();
                                if wanted
                                    .iter()
                                    .any(|u| u.revocation != st.revocation(uid, &app))
                                {
                                    (true, None)
                                } else {
                                    (false, st.grant(uid, &app, Instant::now()))
                                }
                            };
                            if revoked {
                                let _ = bus_tx.send(Msg::ChangeDone {
                                    id,
                                    result: Err(
                                        "locked before the authorization completed".to_string()
                                    ),
                                });
                                continue;
                            }
                            let result = match granted {
                                Some(_) => Ok(()),
                                None => Err("app is no longer protected".to_string()),
                            };
                            let _ = bus_tx.send(Msg::ChangeDone { id, result });
                            let unlocked =
                                state.lock().unwrap().has_ticket(uid, &app, Instant::now());
                            let _ = bus_tx.send(Msg::StateChanged { uid, app, unlocked });
                            continue;
                        }
                        // A launch request: only the execs that waited on this
                        // exact request go ahead. A success that arrives after
                        // the request was removed must not turn into a ticket
                        // for the app.
                        let held =
                            take(&mut pending, |p| p.id == id && p.uid == uid && p.app == app);
                        if held.is_empty() {
                            continue;
                        }
                        // The launch that waited is allowed by the event loop.
                        // For a zero grace period no ticket is stored, so a
                        // second launch asks again. A lock that started after
                        // the execs asked refuses the whole group instead of
                        // letting the answer reopen the app. The state lock
                        // holds the lock's own revocation, so the check and
                        // the grant cannot interleave with it.
                        let revoked = {
                            let mut st = state.lock().unwrap();
                            if held
                                .iter()
                                .any(|p| p.revocation != st.revocation(uid, &app))
                            {
                                true
                            } else {
                                st.grant_for_held_exec(uid, &app, Instant::now());
                                false
                            }
                        };
                        if revoked {
                            for p in &held {
                                activity.resolve(p.id, activity::REVOKED);
                            }
                            for p in held {
                                p.deny();
                            }
                            continue;
                        }
                        allow_held(&state, &activity, uid, &app, held);
                        let unlocked = state.lock().unwrap().has_ticket(uid, &app, Instant::now());
                        let _ = bus_tx.send(Msg::StateChanged { uid, app, unlocked });
                    }
                }
            }
        }
        let mut wait = HOUSEKEEPING;
        let deadlines = pending
            .iter()
            .map(|p| p.deadline)
            .chain(changes.iter().map(|c| c.deadline))
            .chain(unlocks.iter().map(|u| u.deadline));
        if let Some(deadline) = deadlines.min() {
            wait = wait.min(deadline.saturating_duration_since(Instant::now()));
        }
        if let Some(lifetime) = opts.lifetime {
            let left = lifetime.saturating_sub(started.elapsed());
            if left.is_zero() {
                info!("lifetime reached, exiting");
                break;
            }
            wait = wait.min(left + Duration::from_millis(1));
        }
        let r = unsafe { libc::poll(fds.as_mut_ptr(), 3, wait.as_millis() as libc::c_int) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            die("poll", e);
        }
        if fds[0].revents != 0 {
            gate.drain(|req| on_request(&state, &mut pending, &events, &activity, &bus_tx, req))
                .unwrap_or_else(|e| die("fanotify read", e));
        }
        if fds[1].revents != 0 {
            let mut stop = false;
            while let Some(sig) = next_signal(signals) {
                if sig == libc::SIGHUP {
                    // Reloading reads files and asks NSS for uids, which can
                    // block, so it happens on its own thread.
                    let _ = jobs.send(Job::Reload);
                } else {
                    stop = true;
                }
            }
            if stop {
                info!("terminating");
                break;
            }
        }
        if swept.elapsed() >= HOUSEKEEPING {
            swept = Instant::now();
            let expired = state.lock().unwrap().prune(&RealProcs, swept);
            for (uid, app) in expired {
                let _ = bus_tx.send(Msg::StateChanged {
                    uid,
                    app,
                    unlocked: false,
                });
            }
        }
    }
    if let Some(notify) = &notify {
        notify.stopping();
    }
    // Held requests are deliberately left unanswered: closing the fanotify
    // descriptor lets the kernel allow them, as documented in docs/subsystems/launch-gate.md. Forgetting
    // them keeps their Drop from denying.
    mem::forget(pending);
    process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use policy::Why;

    #[test]
    fn why_need_auth_denies() {
        assert!(!Why::NeedAuth.allows());
        assert!(!Why::CallerGone.allows());
        assert!(Why::Ticket.allows());
    }

    #[test]
    fn take_removes_every_match_in_order() {
        let mut list = vec![1, 2, 3, 4, 5];
        let taken = take(&mut list, |n| n % 2 == 0);
        assert_eq!(taken, vec![2, 4]);
        assert_eq!(list, vec![1, 3, 5]);
        assert!(take(&mut list, |_| false).is_empty());
        assert_eq!(list, vec![1, 3, 5]);
    }

    #[test]
    fn an_ending_names_the_activity_kind_and_the_caller_message() {
        assert_eq!(Ending::Cancelled.kind(), activity::CANCELLED);
        assert_eq!(Ending::Denied.kind(), activity::DENIED);
        assert_eq!(Ending::Failed("boom").kind(), activity::FAILED);
        assert_eq!(Ending::Cancelled.message(), "authentication cancelled");
        assert_eq!(Ending::Denied.message(), "authentication was rejected");
        assert_eq!(Ending::Failed("boom").message(), "boom");
    }

    #[test]
    fn a_complete_load_clears_an_earlier_mark_failure() {
        use policy::{AppPolicy, UserPolicy};
        let diag = diag::Diag::new(PathBuf::from("/nonexistent"));
        diag.note_failure();
        assert!(diag.failed() > 0);
        // No users means nothing to mark, so the load is complete.
        note_marks(&diag, &Policy::default());
        assert_eq!(diag.failed(), 0);
        // A listed app with no marked file is a failure even when its path
        // list is empty.
        let mut broken = Policy::default();
        broken.users.insert(
            1,
            UserPolicy {
                name: "u".into(),
                prompt_timeout: Duration::from_secs(60),
                global_grace: Duration::from_secs(300),
                clear_on_lock: true,
                apps: vec![AppPolicy {
                    id: "a".into(),
                    grace: Duration::from_secs(300),
                    autostart: false,
                    relock: Duration::ZERO,
                    paths: Vec::new(),
                    effective: Vec::new(),
                    files: Vec::new(),
                }],
            },
        );
        note_marks(&diag, &broken);
        assert!(diag.failed() > 0);
    }

    fn changes(count: usize, uid: u32) -> Vec<PendingChange> {
        (0..count)
            .map(|i| PendingChange {
                id: i as u64,
                uid,
                app: "app".into(),
                change: Change::Grace(1),
                deadline: Instant::now(),
                revocation: (0, 0),
                settings_revocation: 0,
            })
            .collect()
    }

    fn unlocks(count: usize, uid: u32) -> Vec<PendingUnlock> {
        (0..count)
            .map(|i| PendingUnlock {
                id: i as u64,
                uid,
                deadline: Instant::now(),
                revocation: (0, 0),
            })
            .collect()
    }

    #[test]
    fn held_exec_caps_count_the_app_and_the_user_separately() {
        let none: [(u32, &str); 0] = [];
        assert!(!held_exec_crowded(none.iter().copied(), 1000, "app"));
        // The same app of another user does not fill this user's cap.
        let others: Vec<(u32, &str)> = (0..MAX_HELD_EXECS_PER_USER)
            .map(|_| (2000, "app"))
            .collect();
        assert!(!held_exec_crowded(others.iter().copied(), 1000, "app"));
        // One app at its cap is crowded even when the user total is below it.
        let same_app: Vec<(u32, &str)> =
            (0..MAX_HELD_EXECS_PER_APP).map(|_| (1000, "app")).collect();
        assert!(held_exec_crowded(same_app.iter().copied(), 1000, "app"));
        // The same count spread over other apps leaves room for this one.
        let other_apps: Vec<(u32, &str)> = (0..MAX_HELD_EXECS_PER_APP)
            .map(|_| (1000, "other"))
            .collect();
        assert!(!held_exec_crowded(other_apps.iter().copied(), 1000, "app"));
        // Each app stays under its own cap, so only the user cap can refuse.
        let names = ["a", "b", "c", "d", "e"];
        let spread = |count: usize| -> Vec<(u32, String)> {
            (0..count)
                .map(|i| (1000, names[i % names.len()].to_string()))
                .collect()
        };
        let below = spread(MAX_HELD_EXECS_PER_USER - 1);
        assert!(!held_exec_crowded(
            below.iter().map(|(uid, app)| (*uid, app.as_str())),
            1000,
            "app"
        ));
        let at = spread(MAX_HELD_EXECS_PER_USER);
        assert!(held_exec_crowded(
            at.iter().map(|(uid, app)| (*uid, app.as_str())),
            1000,
            "app"
        ));
    }

    #[test]
    fn auth_caps_share_one_budget_per_user_and_overall() {
        assert!(!auth_crowded(&[], &[], 1000));
        // Four of this user's own requests are crowded, three are not.
        assert!(!auth_crowded(&changes(3, 1000), &[], 1000));
        assert!(auth_crowded(&changes(4, 1000), &[], 1000));
        // Changes and unlocks draw from the same per-user budget.
        assert!(auth_crowded(&changes(2, 1000), &unlocks(2, 1000), 1000));
        assert!(!auth_crowded(&changes(2, 1000), &unlocks(1, 1000), 1000));
        // The overall cap applies to another user's requests too.
        assert!(!auth_crowded(
            &[],
            &unlocks(MAX_WAITING_AUTH - 1, 2000),
            1000
        ));
        assert!(auth_crowded(&[], &unlocks(MAX_WAITING_AUTH, 2000), 1000));
    }
}
