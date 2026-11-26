//! A real KWin session in the virtual backend, driven by the real shisa-agent
//! through a private session bus. The daemon side is a small fake manager on
//! a private system bus. The test skips when the KWin/Wayland toolchain is not
//! installed, so the regular workspace run stays usable on other machines.

use shisa_core::dbus::{
    AppRow, BUS_NAME, INTERFACE, OBJECT_PATH, WINDOW_BUS_NAME, WINDOW_INTERFACE, WINDOW_PATH,
};
use std::collections::HashMap;
use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use zbus::blocking::{Connection, Proxy};

const SOCKET: &str = "wayland-shisa-test";
const WINDOW_TIMEOUT: Duration = Duration::from_secs(10);

const PROBE_SCRIPT: &str = r#"
const SERVICE = "__SERVICE__";
const PATH = "/TestProbe";
const IFACE = "__IFACE__";
let parentSetup = false;
let unrelatedSetup = false;
function snapshot() {
  for (const w of workspace.windowList()) {
    const id = w.desktopFileName || w.resourceClass;
    if (!parentSetup && id === "test.parent") {
      w.minimized = true;
      w.opacity = 0.75;
      w.skipPager = true;
      parentSetup = true;
    }
    if (!unrelatedSetup && id === "test.unrelated") {
      w.minimized = true;
      w.opacity = 0;
      w.skipTaskbar = true;
      w.skipSwitcher = true;
      w.skipPager = true;
      unrelatedSetup = true;
    }
  }
  const list = workspace.windowList().map((w) => ({
    id: String(w.internalId),
    desktopFileName: w.desktopFileName,
    resourceClass: w.resourceClass,
    opacity: w.opacity,
    minimized: w.minimized,
    skipTaskbar: w.skipTaskbar,
    skipSwitcher: w.skipSwitcher,
    skipPager: w.skipPager,
    transient: w.transientFor ? String(w.transientFor.internalId) : "",
  }));
  callDBus(SERVICE, PATH, IFACE, "ProbeReport", JSON.stringify(list), function () {});
}
snapshot();
const timer = new QTimer();
timer.interval = 100;
timer.timeout.connect(snapshot);
timer.start();
"#;

#[derive(Default)]
struct ManagerState {
    apps: Vec<AppRow>,
    allowed: Vec<String>,
}

struct FakeManager {
    state: Arc<Mutex<ManagerState>>,
}

#[zbus::interface(name = "io.github.RiverOnVenus.Shisa.Manager")]
impl FakeManager {
    fn list_apps(&self) -> Vec<AppRow> {
        self.state.lock().unwrap().apps.clone()
    }

    fn window_states(&self) -> Vec<String> {
        self.state.lock().unwrap().allowed.clone()
    }

    fn relock_seconds(&self) -> Vec<(String, u64)> {
        Vec::new()
    }

    fn register_agent(&self) {}
}

struct ProbeReport {
    snapshots: String,
}

struct Probe {
    reports: Arc<Mutex<Vec<ProbeReport>>>,
}

#[zbus::interface(name = "io.github.RiverOnVenus.Shisa.TestProbe")]
impl Probe {
    fn probe_report(&self, snapshots: &str) {
        self.reports.lock().unwrap().push(ProbeReport {
            snapshots: snapshots.to_string(),
        });
    }
}

#[derive(Clone, Debug, PartialEq)]
struct ProbeWindow {
    id: String,
    desktop_file_name: String,
    resource_class: String,
    opacity: f64,
    minimized: bool,
    skip_taskbar: bool,
    skip_switcher: bool,
    skip_pager: bool,
    transient: String,
}

#[derive(Clone, Debug)]
struct JournalEntry {
    opacity: f64,
    minimized: bool,
    skip_taskbar: bool,
    skip_switcher: bool,
    skip_pager: bool,
}

#[derive(Clone)]
struct Original {
    parent: ProbeWindow,
    dialog: ProbeWindow,
    unrelated: ProbeWindow,
}

struct Harness {
    root: PathBuf,
    runtime: PathBuf,
    probe_name: String,
    bus_session: Child,
    bus_system: Child,
    kwin: Option<Child>,
    client: Option<Child>,
    agent: Option<Child>,
    manager: Arc<Mutex<ManagerState>>,
    reports: Arc<Mutex<Vec<ProbeReport>>>,
    session: Connection,
    system: Connection,
    kwin_log: PathBuf,
    client_log: PathBuf,
    agent_log: PathBuf,
    original: Option<Original>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        for child in [
            self.agent.as_mut(),
            self.client.as_mut(),
            self.kwin.as_mut(),
        ]
        .into_iter()
        .flatten()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = self.bus_session.kill();
        let _ = self.bus_session.wait();
        let _ = self.bus_system.kill();
        let _ = self.bus_system.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Harness {
    fn new() -> Result<Option<Self>, String> {
        let missing = missing_tools();
        if !missing.is_empty() {
            eprintln!("skip virtual KWin: missing {}", missing.join(", "));
            return Ok(None);
        }

        let root = env::temp_dir().join(format!("shisa-kwin-virtual-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let runtime = root.join("runtime");
        fs::create_dir_all(&runtime).map_err(|e| e.to_string())?;
        fs::create_dir_all(root.join("home")).map_err(|e| e.to_string())?;
        fs::create_dir_all(root.join("config")).map_err(|e| e.to_string())?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;

        let session_log = root.join("session-bus.log");
        let system_log = root.join("system-bus.log");
        let (mut bus_session, session_addr) = start_bus(&root, "session", "session", &session_log)?;
        let (mut bus_system, system_addr) = match start_bus(&root, "system", "system", &system_log)
        {
            Ok(value) => value,
            Err(error) => {
                let _ = bus_session.kill();
                return Err(error);
            }
        };

        // SAFETY: this integration test binary has one test, and every child
        // process must use the private buses, runtime directory and socket.
        unsafe {
            env::set_var("DBUS_SESSION_BUS_ADDRESS", &session_addr);
            env::set_var("DBUS_SYSTEM_BUS_ADDRESS", &system_addr);
            env::set_var("XDG_RUNTIME_DIR", &runtime);
            env::set_var("WAYLAND_DISPLAY", SOCKET);
            env::set_var("HOME", root.join("home"));
            env::set_var("XDG_CONFIG_HOME", root.join("config"));
            env::set_var("XDG_SESSION_TYPE", "wayland");
            env::set_var("XDG_CURRENT_DESKTOP", "KDE");
            env::remove_var("DISPLAY");
        }

        let manager = Arc::new(Mutex::new(ManagerState {
            apps: vec![("test.parent".to_string(), false, Vec::new(), Vec::new())],
            allowed: Vec::new(),
        }));
        let reports: Arc<Mutex<Vec<ProbeReport>>> = Arc::new(Mutex::new(Vec::new()));
        let probe_name = format!("{BUS_NAME}.TestProbe");

        let system = match zbus::blocking::connection::Builder::address(system_addr.as_str())
            .map_err(|e| e.to_string())
            .and_then(|builder| {
                builder
                    .name(BUS_NAME)
                    .map_err(|e| e.to_string())
                    .and_then(|builder| {
                        builder
                            .serve_at(
                                OBJECT_PATH,
                                FakeManager {
                                    state: manager.clone(),
                                },
                            )
                            .map_err(|e| e.to_string())
                    })
                    .and_then(|builder| builder.build().map_err(|e| e.to_string()))
            }) {
            Ok(connection) => connection,
            Err(error) => {
                let _ = bus_session.kill();
                let _ = bus_system.kill();
                return Err(error);
            }
        };
        let session = match zbus::blocking::connection::Builder::address(session_addr.as_str())
            .map_err(|e| e.to_string())
            .and_then(|builder| {
                builder
                    .name(probe_name.as_str())
                    .map_err(|e| e.to_string())
                    .and_then(|builder| {
                        builder
                            .serve_at(
                                "/TestProbe",
                                Probe {
                                    reports: reports.clone(),
                                },
                            )
                            .map_err(|e| e.to_string())
                    })
                    .and_then(|builder| builder.build().map_err(|e| e.to_string()))
            }) {
            Ok(connection) => connection,
            Err(error) => {
                let _ = bus_session.kill();
                let _ = bus_system.kill();
                return Err(error);
            }
        };

        let mut harness = Harness {
            root,
            runtime,
            probe_name,
            bus_session,
            bus_system,
            kwin: None,
            client: None,
            agent: None,
            manager,
            reports,
            session,
            system,
            kwin_log: PathBuf::new(),
            client_log: PathBuf::new(),
            agent_log: PathBuf::new(),
            original: None,
        };
        harness.kwin_log = harness.root.join("kwin.log");
        harness.client_log = harness.root.join("client.log");
        harness.agent_log = harness.root.join("agent.log");
        harness.start_kwin()?;
        harness.start_client()?;
        harness.load_probe()?;
        harness.original = Some(harness.wait_original()?);
        Ok(Some(harness))
    }

    fn start_kwin(&mut self) -> Result<(), String> {
        let log = File::create(&self.kwin_log).map_err(|e| e.to_string())?;
        let err = log.try_clone().map_err(|e| e.to_string())?;
        let child = Command::new("kwin_wayland")
            .args(["--virtual", "--socket", SOCKET, "--no-lockscreen"])
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err))
            .spawn()
            .map_err(|e| format!("kwin_wayland: {e}"))?;
        self.kwin = Some(child);
        wait_until(Duration::from_secs(15), "the virtual KWin socket", || {
            self.runtime.join(SOCKET).exists()
        })?;
        wait_until(Duration::from_secs(15), "org.kde.KWin", || {
            name_has_owner(&self.session, "org.kde.KWin")
        })?;
        Ok(())
    }

    fn start_client(&mut self) -> Result<(), String> {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("wayland_test_client.c");
        let client = build_client(&fixture, &self.root)?;
        let log = File::create(&self.client_log).map_err(|e| e.to_string())?;
        let err = log.try_clone().map_err(|e| e.to_string())?;
        let child = Command::new(client)
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err))
            .spawn()
            .map_err(|e| format!("wayland-test-client: {e}"))?;
        self.client = Some(child);
        Ok(())
    }

    fn load_probe(&mut self) -> Result<(), String> {
        let script = PROBE_SCRIPT
            .replace("__SERVICE__", &self.probe_name)
            .replace("__IFACE__", "io.github.RiverOnVenus.Shisa.TestProbe");
        let path = self.root.join("probe.js");
        fs::write(&path, script).map_err(|e| e.to_string())?;
        self.load_script(&path, "shisa-test-probe")
    }

    fn load_script(&self, path: &Path, plugin: &str) -> Result<(), String> {
        let path = path.to_str().ok_or("script path is not UTF-8")?;
        let scripting = Proxy::new(
            &self.session,
            "org.kde.KWin",
            "/Scripting",
            "org.kde.kwin.Scripting",
        )
        .map_err(|e| e.to_string())?;
        let id: i32 = scripting
            .call("loadScript", &(path, plugin))
            .map_err(|e| e.to_string())?;
        let object = format!("/Scripting/Script{id}");
        let script = Proxy::new(
            &self.session,
            "org.kde.KWin",
            object.as_str(),
            "org.kde.kwin.Script",
        )
        .map_err(|e| e.to_string())?;
        script.introspect().map_err(|e| e.to_string())?;
        script
            .call::<_, _, ()>("run", &())
            .map_err(|e| e.to_string())
    }

    fn wait_original(&self) -> Result<Original, String> {
        let mut found = None;
        wait_until(WINDOW_TIMEOUT, "the test windows", || {
            if let Some((parent, dialog, unrelated)) = self.probe_state()
                && (parent.opacity - 0.75).abs() < 0.01
                && parent.minimized
                && parent.skip_pager
                && unrelated.opacity == 0.0
                && unrelated.minimized
                && unrelated.skip_taskbar
                && unrelated.skip_switcher
                && unrelated.skip_pager
            {
                found = Some(Original {
                    parent,
                    dialog,
                    unrelated,
                });
                return true;
            }
            false
        })?;
        found.ok_or_else(|| "the test windows did not reach their setup state".into())
    }

    fn start_agent(&mut self) -> Result<(), String> {
        if self.agent.is_some() {
            return Ok(());
        }
        let log = File::create(&self.agent_log).map_err(|e| e.to_string())?;
        let err = log.try_clone().map_err(|e| e.to_string())?;
        let child = Command::new(env!("CARGO_BIN_EXE_shisa-agent"))
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err))
            .spawn()
            .map_err(|e| format!("shisa-agent: {e}"))?;
        self.agent = Some(child);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if name_has_owner(&self.session, WINDOW_BUS_NAME) {
                return Ok(());
            }
            if let Some(child) = self.agent.as_mut()
                && let Ok(Some(status)) = child.try_wait()
            {
                return Err(format!("shisa-agent exited with {status}"));
            }
            if Instant::now() >= deadline {
                return Err("the agent window service did not appear".into());
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn stop_agent(&mut self) {
        if let Some(mut child) = self.agent.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn set_allowed(&self, allowed: &[&str], unlocked: bool) -> Result<(), String> {
        self.manager.lock().unwrap().allowed = allowed.iter().map(|app| app.to_string()).collect();
        self.system
            .emit_signal(
                None::<&str>,
                OBJECT_PATH,
                INTERFACE,
                "StateChanged",
                &("test.parent", unlocked),
            )
            .map_err(|e| e.to_string())
    }

    fn lock(&self) -> Result<(), String> {
        self.set_allowed(&[], false)
    }

    fn allow(&self, app: &str) -> Result<(), String> {
        self.set_allowed(&[app], true)
    }

    fn hidden(&self) -> HashMap<String, u32> {
        let Ok(proxy) = Proxy::new(
            &self.session,
            WINDOW_BUS_NAME,
            WINDOW_PATH,
            WINDOW_INTERFACE,
        ) else {
            return HashMap::new();
        };
        proxy.call("HiddenApps", &()).unwrap_or_default()
    }

    fn wait_hidden(&self, app: &str, count: u32) -> Result<(), String> {
        wait_until(Duration::from_secs(5), "the hidden window count", || {
            self.hidden().get(app).copied() == Some(count)
        })
    }

    fn wait_hidden_empty(&self) -> Result<(), String> {
        wait_until(Duration::from_secs(5), "the empty hidden list", || {
            self.hidden().is_empty()
        })
    }

    fn probe_state(&self) -> Option<(ProbeWindow, ProbeWindow, ProbeWindow)> {
        let json = {
            let reports = self.reports.lock().unwrap();
            reports.last()?.snapshots.clone()
        };
        let windows = probe_windows(&json).ok()?;
        let parent = windows
            .iter()
            .find(|w| w.resource_class == "test.parent" || w.desktop_file_name == "test.parent")?
            .clone();
        let dialog = windows.iter().find(|w| w.transient == parent.id)?.clone();
        let unrelated = windows
            .iter()
            .find(|w| {
                w.resource_class == "test.unrelated" || w.desktop_file_name == "test.unrelated"
            })?
            .clone();
        Some((parent, dialog, unrelated))
    }

    fn wait_pair(
        &self,
        what: &str,
        predicate: impl Fn(&ProbeWindow, &ProbeWindow) -> bool,
    ) -> Result<(ProbeWindow, ProbeWindow), String> {
        let mut found = None;
        wait_until(WINDOW_TIMEOUT, what, || {
            if let Some((parent, dialog, _)) = self.probe_state()
                && predicate(&parent, &dialog)
            {
                found = Some((parent, dialog));
                return true;
            }
            false
        })?;
        found.ok_or_else(|| format!("{what}: no matching window snapshot"))
    }

    fn probe_json(&self) -> String {
        self.reports
            .lock()
            .unwrap()
            .last()
            .map(|report| report.snapshots.clone())
            .unwrap_or_default()
    }

    fn wait_unrelated(&self, original: &ProbeWindow) -> Result<(), String> {
        wait_until(Duration::from_secs(5), "the unrelated window", || {
            self.probe_state()
                .is_some_and(|(_, _, unrelated)| unrelated == *original)
        })
    }

    fn journal(&self) -> Result<HashMap<String, JournalEntry>, String> {
        let path = self.runtime.join("shisa").join("window-snapshots.json");
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => "[]".to_string(),
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        journal_entries(&text)
    }

    fn wait_journal_empty(&self) -> Result<(), String> {
        wait_until(Duration::from_secs(5), "the empty journal", || {
            self.journal().is_ok_and(|entries| entries.is_empty())
        })
    }

    fn log(&self, path: &Path) -> String {
        let text = fs::read_to_string(path).unwrap_or_default();
        let lines: Vec<&str> = text.lines().rev().take(20).collect();
        lines.into_iter().rev().collect::<Vec<_>>().join("\n")
    }

    fn run(&mut self) -> Result<(), String> {
        let original = self.original.clone().ok_or("missing original state")?;
        self.start_agent()?;
        let concealed_pair =
            self.wait_pair("the concealed windows", |p, d| concealed(p) && concealed(d))?;
        let journal = self.journal()?;
        let parent_entry = journal
            .get(&concealed_pair.0.id)
            .ok_or("the parent is missing from the journal")?;
        let dialog_entry = journal
            .get(&concealed_pair.1.id)
            .ok_or("the dialog is missing from the journal")?;
        if !state_matches(&original.parent, parent_entry)
            || !state_matches(&original.dialog, dialog_entry)
        {
            return Err(format!(
                "the journal does not keep the original states: {journal:?}"
            ));
        }
        if journal.contains_key(&original.unrelated.id) {
            return Err("the journal contains the unrelated window".into());
        }
        self.wait_hidden("test.parent", 2)?;
        self.allow("test.parent")?;
        self.wait_pair("the restored windows", |p, d| {
            *p == original.parent && *d == original.dialog
        })?;
        self.wait_hidden_empty()?;
        self.wait_journal_empty()?;
        self.wait_unrelated(&original.unrelated)?;

        // A failed journal write still conceals, and the in-memory originals
        // restore when the app is allowed again.
        let temp = self.runtime.join("shisa").join("window-snapshots.tmp");
        let _ = fs::remove_file(&temp);
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir(&temp).map_err(|e| format!("{}: {e}", temp.display()))?;
        self.lock()?;
        self.wait_pair("the fallback conceal", |p, d| concealed(p) && concealed(d))?;
        self.wait_hidden("test.parent", 2)?;
        if !self.journal()?.is_empty() {
            return Err("the failed save replaced the journal".into());
        }
        fs::remove_dir(&temp).map_err(|e| format!("{}: {e}", temp.display()))?;
        self.allow("test.parent")?;
        self.wait_pair("the restored windows after the failed save", |p, d| {
            *p == original.parent && *d == original.dialog
        })?;
        self.wait_journal_empty()?;

        // The window ids and the journal survive an agent restart.
        self.lock()?;
        let before = self.wait_pair("the concealed windows before the restart", |p, d| {
            concealed(p) && concealed(d)
        })?;
        let journal_before = self.journal()?;
        self.stop_agent();
        thread::sleep(Duration::from_millis(200));
        self.start_agent()?;
        let after = self.wait_pair("the concealed windows after the restart", |p, d| {
            concealed(p) && concealed(d)
        })?;
        if before.0.id != after.0.id || before.1.id != after.1.id {
            return Err("a window got a new internalId across the restart".into());
        }
        let mut before_keys: Vec<String> = journal_before.keys().cloned().collect();
        let mut after_keys: Vec<String> = self.journal()?.keys().cloned().collect();
        before_keys.sort();
        after_keys.sort();
        if before_keys != after_keys {
            return Err("the journal keys changed across the restart".into());
        }
        self.allow("test.parent")?;
        self.wait_pair("the restored windows after the restart", |p, d| {
            *p == original.parent && *d == original.dialog
        })?;
        self.wait_journal_empty()?;

        // A corrupt journal is ignored and the concealed windows migrate
        // back to the defaults.
        self.lock()?;
        self.wait_pair("the concealed windows before corruption", |p, d| {
            concealed(p) && concealed(d)
        })?;
        self.stop_agent();
        let journal_path = self.runtime.join("shisa").join("window-snapshots.json");
        fs::write(&journal_path, "not json")
            .map_err(|e| format!("{}: {e}", journal_path.display()))?;
        self.allow("test.parent")?;
        self.start_agent()?;
        self.wait_pair("the windows migrated from the corrupt journal", |p, d| {
            p.opacity == 1.0 && !p.minimized && d.opacity == 1.0 && !d.minimized
        })?;
        self.wait_journal_empty()?;

        // An oversized journal is ignored the same way.
        self.lock()?;
        self.wait_pair(
            "the concealed windows before the oversized journal",
            |p, d| concealed(p) && concealed(d),
        )?;
        self.stop_agent();
        fs::write(&journal_path, vec![b'x'; 2 * 1024 * 1024])
            .map_err(|e| format!("{}: {e}", journal_path.display()))?;
        self.allow("test.parent")?;
        self.start_agent()?;
        self.wait_pair("the windows migrated from the oversized journal", |p, d| {
            p.opacity == 1.0 && !p.minimized && d.opacity == 1.0 && !d.minimized
        })?;
        self.wait_journal_empty()?;

        // A restart before the unlock loses the in-memory values. The valid
        // empty journal has no entry for the concealed windows, so the new
        // generation records them with the defaults.
        fs::create_dir(&temp).map_err(|e| format!("{}: {e}", temp.display()))?;
        self.lock()?;
        self.wait_pair("the fallback conceal before the restart", |p, d| {
            concealed(p) && concealed(d)
        })?;
        fs::remove_dir(&temp).map_err(|e| format!("{}: {e}", temp.display()))?;
        self.stop_agent();
        self.start_agent()?;
        self.wait_pair("the recovered windows after the restart", |p, d| {
            concealed(p) && concealed(d)
        })?;
        wait_until(Duration::from_secs(5), "the recovered journal", || {
            self.journal().is_ok_and(|entries| !entries.is_empty())
        })?;
        self.allow("test.parent")?;
        self.wait_pair("the recovered windows after the unlock", |p, d| {
            p.opacity == 1.0 && !p.minimized && !p.skip_pager && d.opacity == 1.0 && !d.minimized
        })?;
        self.wait_journal_empty()?;
        self.wait_unrelated(&original.unrelated)?;
        Ok(())
    }
}

#[test]
fn a_virtual_kwin_session_keeps_the_window_journal() {
    let Some(mut harness) = Harness::new().unwrap_or_else(|e| panic!("{e}")) else {
        return;
    };
    if let Err(error) = harness.run() {
        panic!(
            "{error}\n--- journal ---\n{}\n--- probe ---\n{}\n--- kwin ---\n{}\n--- agent ---\n{}\n--- client ---\n{}",
            harness.log(&harness.runtime.join("shisa/window-snapshots.json")),
            harness.probe_json(),
            harness.log(&harness.kwin_log),
            harness.log(&harness.agent_log),
            harness.log(&harness.client_log),
        );
    }
}

fn concealed(window: &ProbeWindow) -> bool {
    window.opacity == 0.0
        && window.minimized
        && window.skip_taskbar
        && window.skip_switcher
        && window.skip_pager
}

fn state_matches(window: &ProbeWindow, entry: &JournalEntry) -> bool {
    (window.opacity - entry.opacity).abs() < 0.001
        && window.minimized == entry.minimized
        && window.skip_taskbar == entry.skip_taskbar
        && window.skip_switcher == entry.skip_switcher
        && window.skip_pager == entry.skip_pager
}

fn probe_windows(json: &str) -> Result<Vec<ProbeWindow>, String> {
    let filter = r#".[] | [(.id // ""), (.desktopFileName // ""), (.resourceClass // ""), ((.opacity // 0)|tostring), ((.minimized // false)|tostring), ((.skipTaskbar // false)|tostring), ((.skipSwitcher // false)|tostring), ((.skipPager // false)|tostring), (.transient // "")] | @tsv"#;
    let output = jq(json, filter)?;
    let mut windows = Vec::new();
    for line in output.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != 9 {
            continue;
        }
        windows.push(ProbeWindow {
            id: fields[0].to_string(),
            desktop_file_name: fields[1].to_string(),
            resource_class: fields[2].to_string(),
            opacity: fields[3].parse().unwrap_or(0.0),
            minimized: fields[4] == "true",
            skip_taskbar: fields[5] == "true",
            skip_switcher: fields[6] == "true",
            skip_pager: fields[7] == "true",
            transient: fields[8].to_string(),
        });
    }
    Ok(windows)
}

fn journal_entries(text: &str) -> Result<HashMap<String, JournalEntry>, String> {
    let filter = r#".[] | [(.[0] // ""), ((.[1].opacity // 0)|tostring), ((.[1].minimized // false)|tostring), ((.[1].skipTaskbar // false)|tostring), ((.[1].skipSwitcher // false)|tostring), ((.[1].skipPager // false)|tostring)] | @tsv"#;
    let output = jq(text, filter)?;
    let mut entries = HashMap::new();
    for line in output.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != 6 {
            continue;
        }
        entries.insert(
            fields[0].to_string(),
            JournalEntry {
                opacity: fields[1].parse().unwrap_or(0.0),
                minimized: fields[2] == "true",
                skip_taskbar: fields[3] == "true",
                skip_switcher: fields[4] == "true",
                skip_pager: fields[5] == "true",
            },
        );
    }
    Ok(entries)
}

fn jq(input: &str, filter: &str) -> Result<String, String> {
    let mut child = Command::new("jq")
        .args(["-r", filter])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("jq: {e}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| "jq stdin".to_string())?
        .write_all(input.as_bytes())
        .map_err(|e| e.to_string())?;
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!("jq: {}", String::from_utf8_lossy(&output.stderr)));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn name_has_owner(connection: &Connection, name: &str) -> bool {
    connection
        .call_method(
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

fn wait_until(
    timeout: Duration,
    what: &str,
    mut condition: impl FnMut() -> bool,
) -> Result<(), String> {
    let start = Instant::now();
    loop {
        if condition() {
            return Ok(());
        }
        if start.elapsed() >= timeout {
            return Err(format!("timed out waiting for {what}"));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn missing_tools() -> Vec<String> {
    let mut missing = Vec::new();
    for tool in ["kwin_wayland", "wayland-scanner", "cc", "dbus-daemon", "jq"] {
        if find_in_path(tool).is_none() {
            missing.push(tool.to_string());
        }
    }
    if find_xdg_shell().is_none() {
        missing.push("xdg-shell.xml".to_string());
    }
    missing
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let paths = env::var_os("PATH")?;
    env::split_paths(&paths)
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

fn find_xdg_shell() -> Option<PathBuf> {
    [
        "/usr/share/wayland-protocols/stable/xdg-shell/xdg-shell.xml",
        "/usr/share/qt6/wayland/protocols/xdg-shell/xdg-shell.xml",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
}

fn start_bus(root: &Path, name: &str, kind: &str, log: &Path) -> Result<(Child, String), String> {
    let socket = root.join(format!("{name}.sock"));
    let config = root.join(format!("{name}.conf"));
    let xml = format!(
        r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>{kind}</type>
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
    fs::write(&config, xml).map_err(|e| e.to_string())?;
    let log = File::create(log).map_err(|e| e.to_string())?;
    let err = log.try_clone().map_err(|e| e.to_string())?;
    let child = Command::new("dbus-daemon")
        .args(["--config-file", config.to_str().ok_or("bus config path")?])
        .arg("--nofork")
        .stdout(Stdio::null())
        .stderr(Stdio::from(err))
        .spawn()
        .map_err(|e| format!("dbus-daemon: {e}"))?;
    if let Err(error) = wait_until(Duration::from_secs(5), "the private bus socket", || {
        socket.exists()
    }) {
        let mut child = child;
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    Ok((child, format!("unix:path={}", socket.display())))
}

fn build_client(fixture: &Path, out: &Path) -> Result<PathBuf, String> {
    let xml = find_xdg_shell().ok_or("xdg-shell.xml is missing")?;
    let header = out.join("xdg-shell-client-protocol.h");
    let code = out.join("xdg-shell-protocol.c");
    run(
        "wayland-scanner",
        &[
            "client-header",
            xml.to_str().ok_or("xdg-shell path")?,
            header.to_str().ok_or("header path")?,
        ],
    )?;
    run(
        "wayland-scanner",
        &[
            "private-code",
            xml.to_str().ok_or("xdg-shell path")?,
            code.to_str().ok_or("code path")?,
        ],
    )?;
    let flags = Command::new("pkg-config")
        .args(["--cflags", "--libs", "wayland-client"])
        .output()
        .map_err(|e| format!("pkg-config: {e}"))?;
    if !flags.status.success() {
        return Err(format!(
            "pkg-config: {}",
            String::from_utf8_lossy(&flags.stderr)
        ));
    }
    let binary = out.join("wayland-test-client");
    let mut cc = Command::new("cc");
    cc.arg("-std=gnu11")
        .arg("-O0")
        .arg("-o")
        .arg(&binary)
        .arg(fixture)
        .arg(&code)
        .arg("-I")
        .arg(out);
    for flag in String::from_utf8_lossy(&flags.stdout).split_whitespace() {
        cc.arg(flag);
    }
    let status = cc.status().map_err(|e| format!("cc: {e}"))?;
    if !status.success() {
        return Err("cc rejected the Wayland test client".into());
    }
    Ok(binary)
}

fn run(tool: &str, args: &[&str]) -> Result<(), String> {
    let status = Command::new(tool)
        .args(args)
        .status()
        .map_err(|e| format!("{tool}: {e}"))?;
    if !status.success() {
        return Err(format!("{tool} failed"));
    }
    Ok(())
}
