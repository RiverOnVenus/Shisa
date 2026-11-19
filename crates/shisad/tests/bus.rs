//! The bus interface against a private dbus-daemon. No root needed: this
//! exercises the served object, the caller identity lookup and the routing of
//! authentication requests to the registered agent.

use shisad::activity;
use shisad::bus::{self, Msg};
use shisad::diag::Diag;
use shisad::events::{Event, Events};
use shisad::policy::Change;
use shisad::policy::{AppPolicy, Policy, State, UserPolicy};
use shisa_core::dbus::ManagerProxyBlocking;
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
            .args(["--config-file".as_ref(), config.as_os_str(), "--nofork".as_ref()])
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
    state
        .lock()
        .unwrap()
        .set_policy(Policy { users: HashMap::from([(uid(), user), (424242, other)]) });
    state
}

#[test]
fn interface_and_settings_changes() {
    // Kept alive for the whole test: dropping it kills the daemon.
    let _bus = PrivateBus::start("iface");
    let state = state();
    let events = Arc::new(Events::new().unwrap());
    let diag = Arc::new(Diag::new(PathBuf::from("/etc/shisa/users")));
    let tx = bus::spawn(
        state.clone(),
        events.clone(),
        Arc::new(activity::Log::default()),
        diag,
    )
    .unwrap();
    // Wait until shisad owns the name.
    wait_for(|| {
        Connection::system().is_ok_and(|conn| {
            ManagerProxyBlocking::new(&conn).is_ok_and(|m| m.list_apps().is_ok())
        })
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
    assert!(result_rx.recv_timeout(Duration::from_secs(5)).unwrap().is_ok());

    // A refused change comes back to the caller as an error.
    let (result_tx, result_rx) = mpsc::channel();
    let caller = conn.clone();
    std::thread::spawn(move || {
        let manager = ManagerProxyBlocking::new(&caller).unwrap();
        let _ = result_tx.send(manager.set_protected("app", true));
    });
    let (id, ..) = wait_change(&events);
    tx.send(Msg::ChangeDone { id, result: Err("not now".into()) }).unwrap();
    assert!(result_rx.recv_timeout(Duration::from_secs(5)).unwrap().is_err());

    // Unsupported options are refused without reaching the event loop.
    let manager = ManagerProxyBlocking::new(&conn).unwrap();
    let bad = manager.set_option("app", "nonsense", zbus::zvariant::Value::from(1u64));
    assert!(bad.is_err());
    // A global option cannot be changed through an app, or the other way
    // round: the authentication box names what the change is for.
    let bad = manager.set_option("app", "clear_on_screen_lock", zbus::zvariant::Value::from(true));
    assert!(bad.is_err());
    let bad = manager.set_option("", "autostart", zbus::zvariant::Value::from(true));
    assert!(bad.is_err());
    // Values the daemon would not use as written never reach the event loop.
    let bad = manager.set_option("", "grace_seconds", zbus::zvariant::Value::from(u64::MAX));
    assert!(bad.is_err());
    let bad = manager.set_option("", "prompt_timeout_seconds", zbus::zvariant::Value::from(600u64));
    assert!(bad.is_err());
    assert!(events.drain().is_empty());
}

fn wait_change(events: &Events) -> (u64, u32, String, Change) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        for event in events.drain() {
            if let Event::ChangeRequest { id, uid, app, change, .. } = event {
                return (id, uid, app, change);
            }
        }
        assert!(Instant::now() < deadline, "no ChangeRequest arrived");
        std::thread::sleep(Duration::from_millis(20));
    }
}
