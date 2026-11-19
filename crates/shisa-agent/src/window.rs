//! The window layer (DESIGN 5.7).
//!
//! KWin runs a generated script that hides the windows of protected apps
//! whose tickets are gone. The script only reaches the session bus, so it
//! calls this agent, and the agent asks shisad on the system bus. The script
//! is regenerated and reloaded whenever the protection or ticket state
//! changes.
//!
//! This layer is best effort: the script and the agent run as the user, and
//! a same-user process can unload the script or change the window properties
//! back. It stops a casual click, not a determined account owner.

use shisa_core::dbus::{WINDOW_BUS_NAME, WINDOW_INTERFACE, WINDOW_PATH};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use tracing::{debug, warn};
use zbus::blocking::{Connection, Proxy};

/// The fixed plugin name older agents used. Unloaded once at startup so its
/// stale handlers do not fight the per-generation scripts.
const LEGACY_PLUGIN: &str = "shisa-window-gate";
/// The focus and relock state shared by the session-bus service and the
/// refresh that follows shisad.
#[derive(Default)]
pub struct Tracker {
    /// App id to its unfocus delay in seconds. Apps with the feature off are
    /// absent, and 1 means hide as soon as the window loses focus.
    pub relock: HashMap<String, u64>,
    /// App id to when its window last lost focus.
    unfocused: HashMap<String, Instant>,
}

impl Tracker {
    /// Apps that have been unfocused longer than their delay. The entry is
    /// removed so the timer fires once per focus cycle.
    pub fn take_due(&mut self, now: Instant) -> Vec<String> {
        let mut due = Vec::new();
        let relock = &self.relock;
        self.unfocused.retain(|app, since| {
            let Some(seconds) = relock.get(app).copied() else {
                return false;
            };
            if seconds == 0 {
                return false;
            }
            let delay = if seconds == 1 {
                Duration::ZERO
            } else {
                Duration::from_secs(seconds)
            };
            if now.saturating_duration_since(*since) >= delay {
                due.push(app.clone());
                false
            } else {
                true
            }
        });
        due
    }

    /// Drop clocks for apps whose windows are not allowed to show. A lock
    /// hides those windows, and a clock started before it must not fire after
    /// the user unlocks and shows the window again.
    pub fn retain_allowed(&mut self, allowed: &[String]) {
        self.unfocused
            .retain(|app, _| allowed.iter().any(|a| a == app));
    }
}

/// Wakes the loader when a freshly loaded script reaches its end and calls
/// back. KWin compiles the source asynchronously, so `run` right after
/// `loadScript` can silently do nothing. This is the proof that the new
/// script really executed and connected its handlers.
#[derive(Default)]
pub struct Ready {
    generation: Mutex<(u64, u32, u32)>,
    changed: Condvar,
}

impl Ready {
    pub fn new() -> Self {
        Ready::default()
    }

    fn note(&self, generation: u64, matched: u32, hidden: u32) {
        let mut current = self.generation.lock().unwrap();
        if generation > current.0 {
            *current = (generation, matched, hidden);
            self.changed.notify_all();
        }
    }

    fn wait(&self, generation: u64, timeout: Duration) -> bool {
        let mut current = self.generation.lock().unwrap();
        let deadline = Instant::now() + timeout;
        while current.0 < generation {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let (guard, result) = self.changed.wait_timeout(current, deadline - now).unwrap();
            current = guard;
            if result.timed_out() && current.0 < generation {
                return false;
            }
        }
        current.0 == generation
    }
}

/// The script reports focus changes and its own generation once it has
/// started. Nothing else crosses the session bus: the window layer does not
/// authenticate.
#[derive(Clone)]
pub struct WindowService {
    actions: std::sync::mpsc::Sender<super::Action>,
    tracker: Arc<Mutex<Tracker>>,
    ready: Arc<Ready>,
}

impl WindowService {
    pub(super) fn new(
        tracker: Arc<Mutex<Tracker>>,
        ready: Arc<Ready>,
        actions: std::sync::mpsc::Sender<super::Action>,
    ) -> Self {
        WindowService {
            tracker,
            ready,
            actions,
        }
    }
}

#[zbus::interface(name = "io.github.RiverOnVenus.Shisa.Window")]
impl WindowService {
    fn windows_checked(&self, generation: &str, matched: &str, hidden: &str) {
        if let (Ok(generation), Ok(matched), Ok(hidden)) =
            (generation.parse(), matched.parse(), hidden.parse())
        {
            self.ready.note(generation, matched, hidden);
        }
    }

    fn verify_lock(&self) -> zbus::fdo::Result<()> {
        self.actions
            .send(super::Action::VerifyLock)
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    /// The app lost or regained focus. The agent starts and cancels its
    /// relock clock when that app has the feature enabled.
    fn window_event(&self, app_id: String, _pid: String, event: String) -> zbus::fdo::Result<bool> {
        let mut tracker = self.tracker.lock().unwrap();
        match event.as_str() {
            // The window lost focus. Start its relock clock when the app has
            // the feature enabled.
            "unfocused" => {
                if tracker.relock.contains_key(&app_id) {
                    tracker.unfocused.insert(app_id, Instant::now());
                }
            }
            // The app has the focus again: a switch back before the delay
            // must cancel the hide, or the window would vanish while the
            // user is working in it.
            "focused" => {
                tracker.unfocused.remove(&app_id);
            }
            _ => {}
        }
        Ok(true)
    }
}

/// Writes the script into the runtime directory and loads it into KWin.
pub struct Gate {
    session: Connection,
    directory: PathBuf,
    loaded: Option<(Vec<String>, Vec<String>)>,
    generation: u64,
    /// KWin only runs a script's handlers when it is loaded under a new
    /// plugin name. Each generation gets its own name, and the last one is
    /// kept in `gate-plugin` so a restarted agent can unload it.
    plugin: String,
    /// Set when a script of a generation has started.
    ready: Arc<Ready>,
    /// Scripts left by earlier generations or agents. They are unloaded only
    /// after a fresh script has reported started: unloading one script right
    /// before loading another makes KWin ignore the new one.
    stale: Vec<String>,
    /// The one-shot activation script currently loaded, if any.
    activation: String,
}

impl Gate {
    /// None when there is no runtime directory to keep the script in. The
    /// agent still runs, the window layer just stays off.
    pub fn new(session: Connection, ready: Arc<Ready>) -> Option<Self> {
        let dir = env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)?;
        let directory = dir.join("shisa");
        if let Err(e) = fs::create_dir_all(&directory) {
            warn!("window gate: {}: {e}", directory.display());
            return None;
        }
        let mut gate = Gate {
            session,
            directory,
            loaded: None,
            // A delayed report from the preceding agent must not satisfy a new check.
            generation: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_micros() as u64,
            plugin: String::new(),
            ready,
            stale: vec![LEGACY_PLUGIN.to_string()],
            activation: String::new(),
        };
        // A previous agent leaves its script behind. Its handlers keep
        // running until a fresh generation is up, because unloading it now
        // would make KWin drop the next load.
        if let Ok(previous) = fs::read_to_string(gate.plugin_file()) {
            let previous = previous.trim();
            if !previous.is_empty() {
                gate.stale.push(previous.to_string());
            }
        }
        Some(gate)
    }

    /// Match the windows to the protected apps and the apps whose windows
    /// are allowed to show. A verification forces a fresh generation and returns
    /// its matched and concealed window counts. Missing reports return None.
    pub fn sync(
        &mut self,
        protected: &[String],
        allowed: &[String],
        verify: bool,
    ) -> Option<(u32, u32)> {
        let mut protected = protected.to_vec();
        let mut allowed = allowed.to_vec();
        protected.sort();
        allowed.sort();
        if !verify && self.loaded.as_ref() == Some(&(protected.clone(), allowed.clone())) {
            // A KWin restart or a manual unload drops the script without
            // telling the agent. Load it again instead of trusting the
            // cached list.
            if self.is_loaded() {
                return None;
            }
            self.loaded = None;
        }
        // Apps the user stopped protecting since the last load. The running
        // script may have hidden their windows, so the new one has to give
        // them back (DESIGN 5.7).
        let restore: Vec<String> = self
            .loaded
            .as_ref()
            .map(|(previous, _)| {
                previous
                    .iter()
                    .filter(|app| !protected.contains(app))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if protected.is_empty() && !verify {
            if restore.is_empty() {
                self.unload();
                self.loaded = Some((protected, allowed));
                return None;
            }
            // Nothing stays protected, but the windows of the dropped apps
            // are still hidden by the running script. Load one generation
            // that gives them back, then stop.
            match self.load(&protected, &allowed, &restore) {
                Ok(()) => {
                    self.unload();
                    self.loaded = Some((protected, allowed));
                }
                Err(LoadError::Missing) => {
                    // No KWin, so nothing is hidden by a script any more.
                    self.loaded = Some((protected, allowed));
                }
                // Keep the previous lists so the next refresh retries; the
                // old script still hides those windows.
                Err(LoadError::Other(why)) => warn!("window gate: {why}"),
            }
            return None;
        }
        match self.load(&protected, &allowed, &restore) {
            Ok(()) => {
                debug!(
                    protected = protected.len(),
                    allowed = allowed.len(),
                    restore = restore.len(),
                    "window gate loaded"
                );
                self.loaded = Some((protected, allowed));
                let report = *self.ready.generation.lock().unwrap();
                return (report.0 == self.generation).then_some((report.1, report.2));
            }
            // KWin is not there, which is normal on other desktops. Ask
            // again on the next refresh in case it just started.
            Err(LoadError::Missing) => debug!("window gate: KWin scripting is not available"),
            Err(LoadError::Other(why)) => warn!("window gate: {why}"),
        }
        None
    }

    /// Bring one app's window to the front after the tray unlocked it. The
    /// gate only changes visibility, so without this the restored window
    /// stays behind whatever the user was looking at.
    pub fn activate(&mut self, app: &str) {
        self.activate_attempt(app, true);
    }

    fn activate_attempt(&mut self, app: &str, retry: bool) {
        self.generation += 1;
        let path = self
            .directory
            .join(format!("shisa-activate-{}.js", self.generation));
        let plugin = format!(
            "shisa-window-activate-{}-{}",
            std::process::id(),
            self.generation
        );
        let script = format!(
            "const TARGET = {};\n\
             const list = workspace.windowList();\n\
             for (let i = 0; i < list.length; i++) {{\n\
               const w = list[i];\n\
               if (w.desktopFileName === TARGET || w.resourceClass === TARGET) {{\n\
                 w.desktops = [workspace.currentDesktop];\n\
                 w.minimized = false;\n\
                 w.opacity = 1;\n\
                 w.skipTaskbar = false;\n\
                 w.skipSwitcher = false;\n\
                 w.skipPager = false;\n\
                 workspace.raiseWindow(w);\n\
                 workspace.activeWindow = w;\n\
                 break;\n\
               }}\n\
             }}\n\
             callDBus({}, {}, {}, \"WindowsChecked\", \"{}\", \"0\", \"0\", function () {{}});\n",
            js_string(app), js_string(WINDOW_BUS_NAME), js_string(WINDOW_PATH),
            js_string(WINDOW_INTERFACE), self.generation
        );
        let result = (|| -> Result<(), String> {
            write_script(&path, &script)?;
            self.run_script(&path, &plugin)
                .map_err(|error| match error {
                    LoadError::Missing => "KWin scripting is not available".to_string(),
                    LoadError::Other(why) => why,
                })?;
            // The previous one-shot is unloaded when the next activation
            // replaces it, or by unload(), not here: an unload immediately
            // after a load can make KWin ignore a gate script loaded right
            // afterwards.
            let previous = std::mem::take(&mut self.activation);
            if !previous.is_empty() {
                self.stale.push(previous);
            }
            self.activation = plugin;
            Ok(())
        })();
        match result {
            // KWin may compile the source after `run` returns, so a working
            // one-shot keeps its file until the next confirmed gate load
            // cleans the old ones up. A failed load never reached the
            // compiler, so its file goes now.
            Ok(()) => {
                if !self.ready.wait(self.generation, Duration::from_millis(1200)) {
                    if retry {
                        self.activate_attempt(app, false);
                    } else {
                        warn!("window activation did not report completed");
                    }
                }
            }
            Err(e) => {
                let _ = fs::remove_file(&path);
                debug!("window gate activate: {e}");
            }
        }
    }

    fn run_script(&self, path: &Path, plugin: &str) -> Result<(), LoadError> {
        let scripting = Proxy::new(
            &self.session,
            "org.kde.KWin",
            "/Scripting",
            "org.kde.kwin.Scripting",
        )
        .map_err(kwin_error)?;
        let path = path
            .to_str()
            .ok_or_else(|| LoadError::Other("script path".into()))?;
        // KWin 6.8 reuses the object path of an unloaded script. Running the
        // returned path can then execute whatever script now owns that ID,
        // often an older gate. Detect the collision before running, and keep
        // the colliding ID occupied so KWin allocates another one.
        let mut reservations = Vec::new();
        let result = (|| {
            for attempt in 0..32 {
                let existing = scripting
                    .introspect()
                    .map_err(|error| kwin_error(error.into()))?;
                let id: i32 = scripting
                    .call("loadScript", &(path, plugin))
                    .map_err(kwin_error)?;
                if id < 0 {
                    return Err(LoadError::Other(format!("KWin refused to load {plugin}")));
                }
                if let Some(object) = new_script_path(&existing, id) {
                    let result = Proxy::new(
                        &self.session,
                        "org.kde.KWin",
                        object.as_str(),
                        "org.kde.kwin.Script",
                    )
                    .and_then(|script| script.call::<_, _, ()>("run", &()))
                    .map_err(kwin_error);
                    if result.is_err() {
                        self.unload_named(plugin);
                    }
                    return result;
                }
                let reservation = format!("{plugin}-reserved-{attempt}");
                let _: i32 = scripting
                    .call("loadScript", &("/dev/null", reservation.as_str()))
                    .inspect_err(|_| self.unload_named(plugin))
                    .map_err(kwin_error)?;
                reservations.push(reservation);
                self.unload_named(plugin);
            }
            Err(LoadError::Other(
                "KWin script IDs are still occupied".into(),
            ))
        })();
        for reservation in reservations {
            self.unload_named(&reservation);
        }
        result
    }

    fn load(
        &mut self,
        protected: &[String],
        allowed: &[String],
        restore: &[String],
    ) -> Result<(), LoadError> {
        // KWin compiles the source asynchronously and `run` can silently do
        // nothing. The script calls back once it has started, and a fresh
        // generation is tried if that does not arrive. A new name also keeps
        // KWin from reusing the old handlers. Old scripts are unloaded only
        // after the new one reports: unloading right before a load makes
        // KWin ignore the next script.
        let mut attempted: Vec<String> = Vec::new();
        for attempt in 0..2 {
            if attempt > 0 {
                debug!("window gate: the script did not report, loading another");
            }
            self.generation += 1;
            let path = self.directory.join(format!("kwin-gate-{}.js", self.generation));
            let source = script_for(protected, allowed, restore, self.generation);
            write_script(&path, &source).map_err(LoadError::Other)?;
            let plugin = format!(
                "shisa-window-gate-{}-{}",
                std::process::id(),
                self.generation
            );
            if let Err(error) = self.run_script(&path, &plugin) {
                for name in attempted {
                    self.unload_named(&name);
                }
                return Err(error);
            }
            attempted.push(plugin);
            if self.ready.wait(self.generation, Duration::from_millis(1200)) {
                // The fresh script is running. Retire everything older.
                let running = attempted.pop().unwrap_or_default();
                let mut retired = std::mem::take(&mut self.stale);
                retired.append(&mut attempted);
                for name in retired {
                    self.unload_named(&name);
                }
                let previous = std::mem::take(&mut self.plugin);
                if !previous.is_empty() {
                    self.unload_named(&previous);
                }
                self.plugin = running;
                let _ = fs::write(self.plugin_file(), &self.plugin);
                self.remove_old_scripts(&path);
                return Ok(());
            }
        }
        // Nothing reported. Leave no half-loaded scripts behind.
        for name in attempted {
            self.unload_named(&name);
        }
        Err(LoadError::Other(
            "the window gate script did not report started".into(),
        ))
    }

    pub fn unload(&mut self) {
        // A one-shot activation script has done its work by now. Leaving it
        // loaded only adds state to KWin's script engine.
        let activation = std::mem::take(&mut self.activation);
        if !activation.is_empty() {
            self.stale.push(activation);
        }
        for name in std::mem::take(&mut self.stale) {
            self.unload_named(&name);
        }
        if self.plugin.is_empty() {
            return;
        }
        self.unload_named(&self.plugin);
        self.plugin.clear();
        let _ = fs::remove_file(self.plugin_file());
    }

    /// Run the current lists again. KWin drops the window changes a script
    /// makes while the screen is locked, so after an unlock the same script
    /// has to be applied once more.
    pub fn force(&mut self) {
        let Some((protected, allowed)) = self.loaded.clone() else {
            return;
        };
        if protected.is_empty() {
            return;
        }
        match self.load(&protected, &allowed, &[]) {
            Ok(()) => debug!("window gate reapplied after unlock"),
            Err(LoadError::Missing) => debug!("window gate: KWin scripting is not available"),
            Err(LoadError::Other(why)) => warn!("window gate: {why}"),
        }
    }

    fn is_loaded(&self) -> bool {
        if self.plugin.is_empty() {
            return false;
        }
        let Ok(scripting) = Proxy::new(
            &self.session,
            "org.kde.KWin",
            "/Scripting",
            "org.kde.kwin.Scripting",
        ) else {
            return false;
        };
        scripting
            .call::<_, _, bool>("isScriptLoaded", &(self.plugin.as_str(),))
            .unwrap_or(false)
    }

    fn plugin_file(&self) -> PathBuf {
        self.directory.join("gate-plugin")
    }

    fn unload_named(&self, name: &str) {
        let Ok(scripting) = Proxy::new(
            &self.session,
            "org.kde.KWin",
            "/Scripting",
            "org.kde.kwin.Scripting",
        ) else {
            return;
        };
        let _: bool = scripting.call("unloadScript", &(name,)).unwrap_or(false);
    }

    /// Older generations are unloaded already. Removing them keeps the
    /// runtime directory from collecting one file per ticket change. A
    /// one-shot activation script had its turn before this generation
    /// compiled, so its source goes too.
    fn remove_old_scripts(&self, current: &Path) {
        let Ok(entries) = fs::read_dir(&self.directory) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let old_gate = name.starts_with("kwin-gate-")
                && name.ends_with(".js")
                && entry.path() != current;
            let old_activation =
                name.starts_with("shisa-activate-") && name.ends_with(".js");
            if old_gate || old_activation {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

enum LoadError {
    /// KWin's scripting interface is not on the session bus.
    Missing,
    Other(String),
}

fn new_script_path(existing: &str, id: i32) -> Option<String> {
    // The introspection document lists one node per script object.
    let name = format!("Script{id}");
    (!existing.contains(&format!("<node name=\"{name}\""))).then(|| format!("/Scripting/{name}"))
}

fn kwin_error(e: zbus::Error) -> LoadError {
    match &e {
        zbus::Error::MethodError(name, _, _)
            if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown"
                || name.as_str() == "org.freedesktop.DBus.Error.NameHasNoOwner" =>
        {
            LoadError::Missing
        }
        _ => LoadError::Other(e.to_string()),
    }
}

fn write_script(path: &Path, script: &str) -> Result<(), String> {
    let tmp = path.with_extension("js.tmp");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    file.write_all(script.as_bytes())
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// The script the agent writes for KWin. The arrays and the generation are
/// the only values that change between loads. `restore` holds apps that were
/// protected at the previous load and whose windows may still be hidden.
fn script_for(
    protected: &[String],
    allowed: &[String],
    restore: &[String],
    generation: u64,
) -> String {
    let list = |ids: &[String]| {
        ids.iter()
            .map(|id| js_string(id))
            .collect::<Vec<_>>()
            .join(", ")
    };
    TEMPLATE
        .replace("@SERVICE@", &js_string(WINDOW_BUS_NAME))
        .replace("@PATH@", &js_string(WINDOW_PATH))
        .replace("@IFACE@", &js_string(WINDOW_INTERFACE))
        .replace("@PROTECTED@", &list(protected))
        .replace("@ALLOWED@", &list(allowed))
        .replace("@RESTORE@", &list(restore))
        .replace("@GENERATION@", &generation.to_string())
}

fn js_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

const TEMPLATE: &str = r#"// Generated by shisa-agent. Do not edit.
const SERVICE = @SERVICE@;
const PATH = @PATH@;
const IFACE = @IFACE@;
const GENERATION = @GENERATION@;
const PROTECTED = [@PROTECTED@];
const ALLOWED = [@ALLOWED@];
const RESTORE = [@RESTORE@];

let hidden = [];

function appOf(w) {
  if (!w) return "";
  const known = function (id) {
    return id && (PROTECTED.indexOf(id) >= 0 || RESTORE.indexOf(id) >= 0);
  };
  if (known(w.desktopFileName)) return w.desktopFileName;
  if (known(w.resourceClass)) return w.resourceClass;
  return "";
}

function isAllowed(app) {
  return ALLOWED.indexOf(app) >= 0;
}

function hide(w) {
  if (hidden.indexOf(w) < 0) hidden.push(w);
  w.skipTaskbar = true;
  w.skipSwitcher = true;
  w.skipPager = true;
  w.opacity = 0;
  w.minimized = true;
}

// What hide() leaves behind. It gives back a window an earlier generation
// hid after the agent restarted, and it tells show() not to undo the
// minimize state of a window the user hid by hand.
function looksHidden(w) {
  return w.opacity === 0 && w.minimized && w.skipTaskbar;
}

function show(w) {
  const i = hidden.indexOf(w);
  // A window the user minimized by hand has opacity 1 and no skip flags.
  // Only a window this layer hid (or an earlier generation hid) comes back
  // up on its own. An unrelated state change must not undo the user's
  // minimize.
  const ours = i >= 0 || looksHidden(w);
  if (i >= 0) hidden.splice(i, 1);
  w.opacity = 1;
  w.skipTaskbar = false;
  w.skipSwitcher = false;
  w.skipPager = false;
  if (ours) w.minimized = false;
}

function consider(w) {
  const app = appOf(w);
  if (!app) return;
  // An app in PROTECTED but not in ALLOWED is locked. An app only in
  // RESTORE was protected at the previous load: its window may still be
  // hidden, so it is shown again.
  if (PROTECTED.indexOf(app) >= 0 && !isAllowed(app)) hide(w);
  else show(w);
}

const list = workspace.windowList();
for (let i = 0; i < list.length; i++) {
  if (appOf(list[i])) consider(list[i]);
  else if (looksHidden(list[i])) show(list[i]);
}
workspace.windowAdded.connect(function (w) {
  consider(w);
});
if (workspace.windowRemoved) {
  workspace.windowRemoved.connect(function (w) {
    const i = hidden.indexOf(w);
    if (i >= 0) hidden.splice(i, 1);
  });
}

// The app that had focus before the current one. Losing focus starts its
// relock clock; gaining it again cancels the clock, so switching back before
// the delay keeps the window visible. Recovering a window that was already
// hidden is the tray's or a relaunch's job.
let activeApp = "";
let activePid = "";
function report(app, pid, event) {
  if (app) callDBus(SERVICE, PATH, IFACE, "WindowEvent", app, pid, event, function () {});
}
const initialWindow = workspace.activeWindow;
if (initialWindow) {
  const initialApp = appOf(initialWindow);
  activeApp = initialApp;
  activePid = initialApp ? "" + initialWindow.pid : "";
  // A relock clock started before this load is cancelled for the window
  // that has the focus now.
  report(activeApp, activePid, "focused");
}
workspace.windowActivated.connect(function (w) {
  const app = appOf(w);
  if (activeApp && activeApp !== app) {
    report(activeApp, activePid, "unfocused");
  }
  report(app, app ? "" + w.pid : "", "focused");
  activeApp = app;
  activePid = app ? "" + w.pid : "";
});

// Tell the agent this generation is running. It waits for this before
// trusting the load, because `run` can reach KWin before the source is
// compiled.
const targets = Array.from(workspace.windowList()).filter(w => {
  const app = appOf(w);
  return PROTECTED.indexOf(app) >= 0 && !isAllowed(app);
});
const concealed = targets.filter(w => looksHidden(w) && w.skipSwitcher && w.skipPager);
callDBus(SERVICE, PATH, IFACE, "WindowsChecked", String(GENERATION), String(targets.length), String(concealed.length), function () {});
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn the_script_carries_every_list() {
        let script = script_for(
            &["org.kde.kwrite".into(), "a\"b".into()],
            &["org.kde.kwrite".into()],
            &["old.app".into()],
            7,
        );
        assert!(script.contains(r#"const PROTECTED = ["org.kde.kwrite", "a\"b"];"#));
        assert!(script.contains(r#"const ALLOWED = ["org.kde.kwrite"];"#));
        assert!(script.contains(r#"const RESTORE = ["old.app"];"#));
        assert!(script.contains("const GENERATION = 7;"));
        assert!(script.contains(&format!("const SERVICE = \"{WINDOW_BUS_NAME}\";")));
        // A reload keeps the focus tracking instead of losing one cycle.
        assert!(script.contains("workspace.activeWindow"));
        // The loader waits for this report before trusting the load.
        assert!(script.contains(r#""WindowsChecked""#));
    }

    #[test]
    fn the_script_cancels_a_relock_when_the_app_regains_focus() {
        let script = script_for(&["app".into()], &[], &[], 1);
        assert!(script.contains(r#""unfocused""#));
        assert!(script.contains(r#""focused""#));
        // A window the user minimized by hand has opacity 1 and must stay
        // minimized across an unrelated reload.
        assert!(script.contains("w.opacity === 0"));
    }

    #[test]
    fn app_ids_are_escaped_for_the_script() {
        assert_eq!(js_string("a\"b\\c"), r#""a\"b\\c""#);
        assert_eq!(js_string("a\nb"), r#""a\nb""#);
    }

    #[test]
    fn a_long_unfocused_window_is_due() {
        let mut tracker = Tracker::default();
        let now = Instant::now();
        tracker.relock.insert("app".into(), 10);
        tracker.unfocused.insert("app".into(), now);
        assert!(tracker.take_due(now + Duration::from_secs(9)).is_empty());
        assert_eq!(
            tracker.take_due(now + Duration::from_secs(10)),
            vec!["app".to_string()]
        );
        // One firing per focus cycle.
        assert!(tracker.take_due(now + Duration::from_secs(20)).is_empty());

        // The immediate preset hides on the first look.
        tracker.relock.insert("now".into(), 1);
        tracker.unfocused.insert("now".into(), now);
        assert_eq!(tracker.take_due(now), vec!["now".to_string()]);
    }

    #[test]
    fn the_immediate_preset_fires_and_a_disabled_app_is_dropped() {
        let mut tracker = Tracker::default();
        let now = Instant::now();
        tracker.relock.insert("off".into(), 5);
        tracker.unfocused.insert("off".into(), now);
        // The user turns the feature off while the window is unfocused.
        tracker.relock.remove("off");
        assert!(tracker.take_due(now + Duration::from_secs(9)).is_empty());
        assert!(tracker.unfocused.is_empty());
    }

    #[test]
    fn a_lock_drops_the_clock_of_the_hidden_window() {
        let mut tracker = Tracker::default();
        let now = Instant::now();
        tracker.relock.insert("shown".into(), 60);
        tracker.relock.insert("hidden".into(), 60);
        tracker.unfocused.insert("shown".into(), now);
        tracker.unfocused.insert("hidden".into(), now);
        // The user locked "hidden"; unlocking it later must not inherit the
        // old clock.
        tracker.retain_allowed(&["shown".to_string()]);
        assert!(tracker.unfocused.contains_key("shown"));
        assert!(!tracker.unfocused.contains_key("hidden"));
    }

    #[test]
    fn a_reused_kwin_id_does_not_point_to_the_new_script() {
        let existing = r#"<node><node name="Script11"/><node name="Script12"/></node>"#;
        assert_eq!(new_script_path(existing, 11), None);
        assert_eq!(
            new_script_path(existing, 1).as_deref(),
            Some("/Scripting/Script1")
        );
    }

    #[test]
    fn a_started_script_wakes_only_its_own_generation() {
        let ready = Ready::new();
        assert!(!ready.wait(1, Duration::from_millis(20)));
        let ready = Arc::new(ready);
        let waiter = ready.clone();
        let handle = thread::spawn(move || waiter.wait(2, Duration::from_millis(500)));
        thread::sleep(Duration::from_millis(20));
        ready.note(2, 3, 2);
        ready.note(1, 0, 0);
        assert_eq!(*ready.generation.lock().unwrap(), (2, 3, 2));
        assert!(!ready.wait(1, Duration::ZERO));
        assert!(handle.join().unwrap());
        assert!(ready.wait(2, Duration::from_millis(1)));
    }

    #[test]
    fn regaining_focus_cancels_the_pending_hide() {
        let tracker = Arc::new(Mutex::new(Tracker::default()));
        tracker.lock().unwrap().relock.insert("app".into(), 5);
        let service = WindowService::new(
            tracker.clone(),
            Arc::new(Ready::new()),
            std::sync::mpsc::channel().0,
        );
        assert!(
            service
                .window_event("app".into(), "1".into(), "unfocused".into())
                .unwrap()
        );
        assert!(tracker.lock().unwrap().unfocused.contains_key("app"));
        assert!(
            service
                .window_event("app".into(), "1".into(), "focused".into())
                .unwrap()
        );
        assert!(!tracker.lock().unwrap().unfocused.contains_key("app"));
        // The timer never fires for this focus cycle.
        assert!(
            tracker
                .lock()
                .unwrap()
                .take_due(Instant::now() + Duration::from_secs(60))
                .is_empty()
        );
    }

    #[test]
    fn only_an_unfocused_event_starts_the_clock() {
        let tracker = Arc::new(Mutex::new(Tracker::default()));
        tracker.lock().unwrap().relock.insert("app".into(), 5);
        let service = WindowService::new(
            tracker.clone(),
            Arc::new(Ready::new()),
            std::sync::mpsc::channel().0,
        );
        assert!(
            service
                .window_event("app".into(), "1".into(), "unfocused".into())
                .unwrap()
        );
        assert!(tracker.lock().unwrap().unfocused.contains_key("app"));
        // An app without the feature is not tracked at all.
        assert!(
            service
                .window_event("other".into(), "1".into(), "unfocused".into())
                .unwrap()
        );
        assert!(!tracker.lock().unwrap().unfocused.contains_key("other"));
        // Other events do not touch the clock.
        tracker.lock().unwrap().unfocused.remove("app");
        assert!(
            service
                .window_event("app".into(), "1".into(), "activated".into())
                .unwrap()
        );
        assert!(tracker.lock().unwrap().unfocused.is_empty());
    }
}
