//! The bus interface against a private dbus-daemon. No root needed: this
//! exercises the served object, the caller identity lookup and the routing of
//! authentication requests to the registered agent.

use shisa_core::config::MAX_PROTECTED_PATHS;
use shisa_core::dbus::ManagerProxyBlocking;
use shisad::activity;
use shisad::bus::{self, Msg};
use shisad::diag::Diag;
use shisad::events::{Event, Events};
use shisad::policy::Change;
use shisad::policy::{AppPolicy, Policy, State, UserPolicy};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
use zbus::blocking::Connection;

struct PrivateBus {
    dir: PathBuf,
    daemon: Child,
}

/// Only one test may own DBUS_SYSTEM_BUS_ADDRESS at a time.
static ONE_BUS: Mutex<()> = Mutex::new(());

impl PrivateBus {
    fn start(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("shisad-bus-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("bus.sock");
        let config = dir.join("bus.conf");
        let xml = format!(
            r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>system</type>
  <listen>unix:path={}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow user="*"/>
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
            socket.display()
        );
        std::fs::write(&config, xml).unwrap();
        let daemon = Command::new("dbus-daemon")
            .args([
                "--config-file".as_ref(),
                config.as_os_str(),
                "--nofork".as_ref(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("dbus-daemon");
        let address = format!("unix:path={}", socket.display());
        // The test binary starts with no bus connection in use yet.
        unsafe { std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", &address) };
        wait_for(|| socket.exists());
        PrivateBus { dir, daemon }
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn uid() -> u32 {
    unsafe { libc::getuid() }
}

fn state() -> Arc<Mutex<State>> {
    let app = AppPolicy {
        id: "app".into(),
        grace: Duration::from_secs(300),
        autostart: false,
        relock: Duration::ZERO,
        paths: vec!["/usr/bin/app".into()],
        effective: vec!["/usr/bin/app".into()],
        files: Vec::new(),
    };
    let user = UserPolicy {
        name: "tester".into(),
        prompt_timeout: Duration::from_secs(5),
        global_grace: Duration::from_secs(300),
        clear_on_lock: true,
        apps: vec![app],
    };
    // 424242 stands in for a user without an agent.
    let other = UserPolicy {
        name: "nobody".into(),
        prompt_timeout: Duration::from_secs(5),
        global_grace: Duration::from_secs(300),
        clear_on_lock: true,
        apps: Vec::new(),
    };
    let state = Arc::new(Mutex::new(State::default()));
    state.lock().unwrap().set_policy(Policy {
        users: HashMap::from([(uid(), user), (424242, other)]),
    });
    state
}

#[test]
fn interface_and_settings_changes() {
    // Kept alive for the whole test: dropping it kills the daemon.
    let _serial = ONE_BUS.lock().unwrap_or_else(|e| e.into_inner());
    let _bus = PrivateBus::start("iface");
    let state = state();
    let events = Arc::new(Events::new().unwrap());
    let diag = Arc::new(Diag::new(PathBuf::from("/etc/shisa/users")));
    let tx = bus::spawn(
        state.clone(),
        events.clone(),
        Arc::new(activity::Log::default()),
        diag,
        Arc::new(Mutex::new(())),
    )
    .unwrap();
    // Wait until shisad owns the name.
    wait_for(|| {
        Connection::system()
            .is_ok_and(|conn| ManagerProxyBlocking::new(&conn).is_ok_and(|m| m.list_apps().is_ok()))
    });
    let conn = Connection::system().unwrap();
    let manager = ManagerProxyBlocking::new(&conn).unwrap();
    let apps = manager.list_apps().unwrap();
    assert_eq!(
        apps,
        vec![(
            "app".to_string(),
            false,
            vec!["/usr/bin/app".to_string()],
            vec!["/usr/bin/app".to_string()]
        )]
    );
    // The activity page asks for the caller's launches; a fresh daemon has
    // none.
    assert_eq!(manager.recent_launches().unwrap(), (vec![], 0, 0, 0));
    // The diagnostics card reads the daemon's own numbers. The counts are the
    // caller's policy, the config file is the one that policy came from, and
    // the account check only says whether it could read /etc/shadow.
    let diag = manager.diagnostics().unwrap();
    assert_eq!(diag.0, env!("CARGO_PKG_VERSION"));
    assert!(diag.1 > 0);
    assert_eq!((diag.4, diag.5), (1, 1));
    assert_eq!(diag.6, "/etc/shisa/users/tester.toml");
    assert_eq!(diag.8, 0);
    manager.lock("app").unwrap();
    manager.lock("").unwrap();
    assert_eq!(manager.global_config().unwrap(), (300, true, 5));

    // A settings change waits until the event loop reports the result.
    let (result_tx, result_rx) = mpsc::channel();
    let caller = conn.clone();
    std::thread::spawn(move || {
        let manager = ManagerProxyBlocking::new(&caller).unwrap();
        let _ = result_tx.send(manager.set_protected("app", false));
    });
    let (id, change_uid, app, change) = wait_change(&events);
    assert_eq!(change_uid, uid());
    assert_eq!(app, "app");
    assert_eq!(change, Change::Protected(false));
    tx.send(Msg::ChangeDone { id, result: Ok(()) }).unwrap();
    assert!(
        result_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_ok()
    );

    // A refused change comes back to the caller as an error.
    let (result_tx, result_rx) = mpsc::channel();
    let caller = conn.clone();
    std::thread::spawn(move || {
        let manager = ManagerProxyBlocking::new(&caller).unwrap();
        let _ = result_tx.send(manager.set_protected("app", true));
    });
    let (id, ..) = wait_change(&events);
    tx.send(Msg::ChangeDone {
        id,
        result: Err("not now".into()),
    })
    .unwrap();
    assert!(
        result_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_err()
    );

    // Unsupported options are refused without reaching the event loop.
    let manager = ManagerProxyBlocking::new(&conn).unwrap();
    let bad = manager.set_option("app", "nonsense", zbus::zvariant::Value::from(1u64));
    assert!(bad.is_err());
    // A global option cannot be changed through an app, or the other way
    // round: the authentication box names what the change is for.
    let bad = manager.set_option(
        "app",
        "clear_on_screen_lock",
        zbus::zvariant::Value::from(true),
    );
    assert!(bad.is_err());
    let bad = manager.set_option("", "autostart", zbus::zvariant::Value::from(true));
    assert!(bad.is_err());
    // Values the daemon would not use as written never reach the event loop.
    let bad = manager.set_option("", "grace_seconds", zbus::zvariant::Value::from(u64::MAX));
    assert!(bad.is_err());
    let bad = manager.set_option(
        "",
        "prompt_timeout_seconds",
        zbus::zvariant::Value::from(600u64),
    );
    assert!(bad.is_err());
    let many: Vec<String> = (0..=MAX_PROTECTED_PATHS)
        .map(|i| format!("/tmp/shisa-path-{i}"))
        .collect();
    let bad = manager.set_option("app", "paths", zbus::zvariant::Value::from(many));
    assert!(bad.is_err());
    assert!(events.drain().is_empty());
}

fn wait_change(events: &Events) -> (u64, u32, String, Change) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        for event in events.drain() {
            if let Event::ChangeRequest {
                id,
                uid,
                app,
                change,
                ..
            } = event
            {
                return (id, uid, app, change);
            }
        }
        assert!(Instant::now() < deadline, "no ChangeRequest arrived");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_unlock(events: &Events) -> (u64, u32, String) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        for event in events.drain() {
            if let Event::UnlockRequest { id, uid, app, .. } = event {
                return (id, uid, app);
            }
        }
        assert!(Instant::now() < deadline, "no UnlockRequest arrived");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn start_daemon(state: Arc<Mutex<State>>) -> (Arc<Events>, mpsc::Sender<Msg>) {
    let events = Arc::new(Events::new().unwrap());
    let diag = Arc::new(Diag::new(PathBuf::from("/etc/shisa/users")));
    let tx = bus::spawn(
        state,
        events.clone(),
        Arc::new(activity::Log::default()),
        diag,
        Arc::new(Mutex::new(())),
    )
    .unwrap();
    wait_for(|| {
        Connection::system()
            .is_ok_and(|conn| ManagerProxyBlocking::new(&conn).is_ok_and(|m| m.list_apps().is_ok()))
    });
    (events, tx)
}

#[test]
fn window_states_and_relock_seconds_follow_the_policy() {
    let _serial = ONE_BUS.lock().unwrap_or_else(|e| e.into_inner());
    let _bus = PrivateBus::start("states");
    let state = state();
    {
        let mut state = state.lock().unwrap();
        let mut policy = state.policy().clone();
        policy.users.get_mut(&uid()).unwrap().apps[0].relock = Duration::from_secs(300);
        state.set_policy(policy);
        state.allow_window(uid(), "app");
    }
    let (_events, _tx) = start_daemon(state);
    let conn = Connection::system().unwrap();
    let manager = ManagerProxyBlocking::new(&conn).unwrap();
    assert_eq!(manager.window_states().unwrap(), vec!["app"]);
    assert_eq!(
        manager.relock_seconds().unwrap(),
        vec![("app".to_string(), 300)]
    );
    // Locking drops the window allowance, not the relock setting.
    manager.lock("").unwrap();
    assert!(manager.window_states().unwrap().is_empty());
    assert_eq!(
        manager.relock_seconds().unwrap(),
        vec![("app".to_string(), 300)]
    );
}

#[test]
fn a_lock_starts_a_new_generation_and_a_single_app_lock_stays_scoped() {
    let _serial = ONE_BUS.lock().unwrap_or_else(|e| e.into_inner());
    let _bus = PrivateBus::start("revocation");
    let state = state();
    let (_events, _tx) = start_daemon(state.clone());
    let conn = Connection::system().unwrap();
    let manager = ManagerProxyBlocking::new(&conn).unwrap();

    let before = state.lock().unwrap().revocation(uid(), "app");
    // An invalid id is refused instead of becoming a revocation key.
    assert!(manager.lock("bad/id").is_err());
    // A valid id that protects nothing is a no-op and starts no generation,
    // so repeated locks cannot grow the map with unprotected apps.
    let untouched = state.lock().unwrap().revocation(uid(), "not-protected");
    manager.lock("not-protected").unwrap();
    assert_eq!(
        state.lock().unwrap().revocation(uid(), "not-protected"),
        untouched
    );
    manager.lock("app").unwrap();
    let after = state.lock().unwrap().revocation(uid(), "app");
    assert_ne!(before.1, after.1, "a single-app lock must bump the app");
    assert_eq!(before.0, after.0, "it must leave other apps' locks alone");

    manager.lock("").unwrap();
    let global = state.lock().unwrap().revocation(uid(), "app");
    assert_ne!(after.0, global.0, "a global lock must bump the user");
    assert_eq!(after.1, global.1, "it must not bump the app generation");
}

#[test]
fn hide_keeps_the_ticket_and_unhide_requires_it() {
    let _serial = ONE_BUS.lock().unwrap_or_else(|e| e.into_inner());
    let _bus = PrivateBus::start("hide");
    let state = state();
    state.lock().unwrap().allow_window(uid(), "app");
    let (_events, _tx) = start_daemon(state.clone());
    let conn = Connection::system().unwrap();
    let manager = ManagerProxyBlocking::new(&conn).unwrap();

    // Without a ticket, hiding drops the window permission and restoring it
    // has to ask for authentication.
    manager.hide("app").unwrap();
    assert!(manager.window_states().unwrap().is_empty());
    assert!(!manager.unhide("app").unwrap());
    assert!(manager.window_states().unwrap().is_empty());

    // A ticket survives the hide, and unhide uses it instead of a prompt.
    state.lock().unwrap().grant(uid(), "app", Instant::now());
    manager.hide("app").unwrap();
    assert!(
        manager
            .list_apps()
            .unwrap()
            .iter()
            .any(|row| row.0 == "app" && row.1)
    );
    assert!(manager.unhide("app").unwrap());
    assert_eq!(manager.window_states().unwrap(), vec!["app"]);

    // A manual lock still drops the ticket, so unhide cannot bypass it.
    manager.hide("app").unwrap();
    manager.lock("app").unwrap();
    assert!(!manager.unhide("app").unwrap());
    assert!(manager.window_states().unwrap().is_empty());

    // Only Lock addresses every app.
    assert!(manager.hide("").is_err());
    assert!(manager.hide("bad/id").is_err());
    assert!(manager.unhide("bad/id").is_err());
}

#[test]
fn hide_reports_the_ticket_it_keeps() {
    let _serial = ONE_BUS.lock().unwrap_or_else(|e| e.into_inner());
    let _bus = PrivateBus::start("hide-signal");
    let state = state();
    let control = state.clone();
    let (_events, _tx) = start_daemon(state);

    let observer = Connection::system().unwrap();
    ManagerProxyBlocking::new(&observer)
        .unwrap()
        .register_agent()
        .unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (signal_tx, signal_rx) = mpsc::channel();
    let watcher = std::thread::spawn(move || {
        let agent = ManagerProxyBlocking::new(&observer).unwrap();
        let mut signals = agent.receive_state_changed().unwrap();
        ready_tx.send(()).unwrap();
        let seen = signals.next().map(|signal| {
            let args = signal.args().unwrap();
            (args.app_id, args.unlocked)
        });
        let _ = signal_tx.send(seen);
    });
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    let caller = Connection::system().unwrap();
    let manager = ManagerProxyBlocking::new(&caller).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    // The match rule reaches the bus asynchronously. Grant a ticket and hide
    // again until the observer sees the signal.
    let seen = loop {
        control.lock().unwrap().grant(uid(), "app", Instant::now());
        manager.hide("app").unwrap();
        match signal_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(seen)) => break seen,
            Ok(None) => panic!("the signal stream ended"),
            Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => continue,
            Err(e) => panic!("no StateChanged arrived: {e}"),
        }
    };
    // The hide removed the window permission, not the ticket it reports.
    assert_eq!(seen, ("app".to_string(), true));
    assert!(manager.window_states().unwrap().is_empty());
    assert!(
        manager
            .list_apps()
            .unwrap()
            .iter()
            .any(|row| row.0 == "app" && row.1)
    );
    watcher.join().unwrap();
}

#[test]
fn unlock_waits_for_authentication_and_rejects_unknown_apps() {
    let _serial = ONE_BUS.lock().unwrap_or_else(|e| e.into_inner());
    let _bus = PrivateBus::start("unlock");
    let state = state();
    let (events, tx) = start_daemon(state);
    let conn = Connection::system().unwrap();
    let manager = ManagerProxyBlocking::new(&conn).unwrap();

    // These never reach the event loop, so no authentication box is started.
    assert!(manager.unlock("not-protected").is_err());
    assert!(manager.unlock("bad/id").is_err());
    assert!(events.drain().is_empty());

    let (result_tx, result_rx) = mpsc::channel();
    let caller = conn.clone();
    std::thread::spawn(move || {
        let manager = ManagerProxyBlocking::new(&caller).unwrap();
        let _ = result_tx.send(manager.unlock("app"));
    });
    let (id, unlock_uid, app) = wait_unlock(&events);
    assert_eq!(unlock_uid, uid());
    assert_eq!(app, "app");
    tx.send(Msg::ChangeDone { id, result: Ok(()) }).unwrap();
    assert!(
        result_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_ok()
    );
}

#[test]
fn a_registered_agent_receives_state_changes() {
    let _serial = ONE_BUS.lock().unwrap_or_else(|e| e.into_inner());
    let _bus = PrivateBus::start("agent");
    let state = state();
    let control = state.clone();
    state.lock().unwrap().allow_window(uid(), "app");
    let (_events, _tx) = start_daemon(state);

    let observer = Connection::system().unwrap();
    let register = ManagerProxyBlocking::new(&observer).unwrap();
    register.register_agent().unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (signal_tx, signal_rx) = mpsc::channel();
    let watcher = std::thread::spawn(move || {
        let agent = ManagerProxyBlocking::new(&observer).unwrap();
        let mut signals = agent.receive_state_changed().unwrap();
        ready_tx.send(()).unwrap();
        let seen = signals.next().map(|signal| {
            let args = signal.args().unwrap();
            (args.app_id, args.unlocked)
        });
        let _ = signal_tx.send(seen);
    });
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    let caller = Connection::system().unwrap();
    let manager = ManagerProxyBlocking::new(&caller).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    // The match rule reaches the bus asynchronously. Repeat the state change
    // until the observer sees it instead of sleeping for a fixed time.
    let seen = loop {
        control.lock().unwrap().allow_window(uid(), "app");
        manager.lock("app").unwrap();
        match signal_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(seen)) => break seen,
            Ok(None) => panic!("the signal stream ended"),
            Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => continue,
            Err(e) => panic!("no StateChanged arrived: {e}"),
        }
    };
    assert_eq!(seen, ("app".to_string(), false));
    watcher.join().unwrap();
}

#[test]
fn a_global_or_screen_lock_ends_registered_settings_windows() {
    let _serial = ONE_BUS.lock().unwrap_or_else(|e| e.into_inner());
    let _bus = PrivateBus::start("settings-lock");
    let (_events, tx) = start_daemon(state());

    let observer = Connection::system().unwrap();
    let manager = ManagerProxyBlocking::new(&observer).unwrap();

    // The match rule reaches the bus asynchronously. Repeat the global lock
    // until the observer sees the signal.
    let (seen_tx, seen_rx) = mpsc::channel();
    let watch_conn = observer.clone();
    let watcher = std::thread::spawn(move || {
        let manager = ManagerProxyBlocking::new(&watch_conn).unwrap();
        let mut signals = manager.receive_settings_locked().unwrap();
        let _ = seen_tx.send(signals.next().is_some());
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        manager.register_settings().unwrap();
        manager.lock("").unwrap();
        match seen_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(true) => break,
            Ok(false) => panic!("the signal stream ended"),
            Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => continue,
            Err(e) => panic!("no SettingsLocked arrived: {e}"),
        }
    }
    watcher.join().unwrap();

    // The same connection is now in the locked set. A change that misses the
    // exit signal is refused instead of reaching its retained authorization.
    let (result_tx, result_rx) = mpsc::channel();
    let caller = observer.clone();
    std::thread::spawn(move || {
        let manager = ManagerProxyBlocking::new(&caller).unwrap();
        let _ = result_tx.send(manager.set_protected("app", false));
    });
    let refused = result_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the change was not refused");
    assert!(refused.unwrap_err().to_string().contains("settings locked"));

    // The screen-lock path arrives as a daemon message and uses the same
    // registry. The first lock consumed it, so register on every try.
    let (seen_tx, seen_rx) = mpsc::channel();
    let watch_conn = observer.clone();
    let watcher = std::thread::spawn(move || {
        let manager = ManagerProxyBlocking::new(&watch_conn).unwrap();
        let mut signals = manager.receive_settings_locked().unwrap();
        let _ = seen_tx.send(signals.next().is_some());
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        manager.register_settings().unwrap();
        tx.send(Msg::SettingsLocked { uid: uid() }).unwrap();
        match seen_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(true) => break,
            Ok(false) => panic!("the signal stream ended"),
            Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => continue,
            Err(e) => panic!("no screen-lock SettingsLocked arrived: {e}"),
        }
    }
    watcher.join().unwrap();
}
