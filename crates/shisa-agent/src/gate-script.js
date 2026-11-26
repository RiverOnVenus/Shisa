// Template for the script shisa-agent writes to KWin. script_for fills the
// placeholders. Edit this file to change gate behavior.
const SERVICE = "@SERVICE@";
const PATH = "@PATH@";
const IFACE = "@IFACE@";
let generation = /* @GENERATION@ */ 0;
const instance = generation;
let PROTECTED = /* @PROTECTED@ */ [];
let ALLOWED = /* @ALLOWED@ */ [];
let RESTORE = /* @RESTORE@ */ [];

// internalId identifies one KWin window for its lifetime, including across
// agent restarts. Capture the whole batch before minimizing any parent.
const concealedState = {
  skipTaskbar: true,
  skipSwitcher: true,
  skipPager: true,
  opacity: 0,
  minimized: true,
};
const legacyOriginal = {
  skipTaskbar: false,
  skipSwitcher: false,
  skipPager: false,
  opacity: 1,
  minimized: false,
};
// A snapshot the agent cannot persist must not leave a locked window visible.
// After these attempts the script conceals with the captured values still in
// memory; a later generation falls back to the legacy defaults.
const SAVE_RETRY_MS = 500;
const SAVE_ATTEMPTS = 3;
// A parent restore can change a transient after the setters return. Keep the
// original values and retry for a short window before giving up.
const RESTORE_RETRY_MS = 250;
const RESTORE_MAX_MS = 5000;

// The journal is user-writable, so a corrupt or partial file must not stop
// the script. Keep only entries that match the captured state's shape.
//
// KWin runs this script in QJSEngine, which rejects the optional catch
// binding (`catch {`). Every catch here must name its binding.
function loadOriginals(text) {
  try {
    const parsed = JSON.parse(text);
    const map = new Map();
    if (!Array.isArray(parsed)) return map;
    for (const entry of parsed) {
      if (!Array.isArray(entry) || entry.length !== 2) continue;
      const [id, state] = entry;
      if (typeof id !== "string" || !state || typeof state !== "object")
        continue;
      const original = {};
      let entryOk = true;
      for (const key of Object.keys(concealedState)) {
        const value = state[key];
        if (
          key === "opacity"
            ? typeof value !== "number"
            : typeof value !== "boolean"
        ) {
          entryOk = false;
          break;
        }
        original[key] = value;
      }
      if (entryOk) map.set(id, original);
    }
    return map;
  } catch (error) {
    return new Map();
  }
}
let originals = loadOriginals(/* @SNAPSHOTS@ */ "[]");
// A generation can start with a window this layer concealed before its
// journal write failed, so an otherwise valid journal has no entry for it.
// Recover known-app windows with the defaults until the journal matches, or
// an unlock would never show them again. A window that appears later is left
// alone.
let recoverUnknown = true;
let pendingSave;
let saveStarted = 0;
let saveAttempts = 0;
let restorePending = false;
let restoreRetryAt = 0;
let restoreStarted = 0;
let restoreAttempts = 0;
let sequence = 0;
let retryAt = 0;
let requestedForeground = "";
let inventorySequence = 0;
let lastSnapshot;
// The protected apps that had at least one window at the previous pass. A
// generation starts with null so the first pass records what was already
// open instead of reporting every window as closed.
let presentApps = null;
// Reports the agent has not accepted yet. A rejected report is sent again on
// the next poll. Declared before the first possible report call.
const pendingReports = new Map();
let reportSequence = 0;

// A protected app that loses its last window can be reopened from its own
// tray icon without an exec, so the launch gate never sees it. Report the
// close once so the agent can drop the app's window permission and keep its
// ticket: a reopen within the grace period still avoids the password, and a
// later one goes through the restore panel. The report goes out even when
// the app is not allowed at this instant, because the script's window state
// can lag a grant, and `Hide` is a no-op without permission.
function reportClosedApps() {
  const now = new Set();
  for (const window of workspace.windowList()) {
    const app = appOf(window);
    // A window KWin still lists but marks hidden is not one the user can
    // see, so the app counts as closed for the same purpose.
    if (app && PROTECTED.includes(app) && !window.hidden) now.add(app);
  }
  const before = presentApps;
  presentApps = now;
  if (before === null) return;
  for (const app of before) {
    if (now.has(app) || !PROTECTED.includes(app)) continue;
    // The daemon rejects malformed ids, but they must not reach the bus or a
    // log line in the first place. The line-based reports use the same guard.
    if (/[\x00\r\n]/.test(app)) continue;
    // The action queue can be full behind a slow authentication prompt.
    // Reuse the report retry so a rejected close is sent again instead of
    // leaving the app with window permission.
    report(app, "", "closed");
  }
}

function reportHiddenWindows() {
  const counts = new Map();
  for (const window of workspace.windowList()) {
    if (!originals.has(String(window.internalId)) || !locked(window)) continue;
    const app = appOf(window);
    if (!app || /[\x00\r\n]/.test(app)) continue;
    if (isConcealed(window)) {
      counts.set(app, (counts.get(app) || 0) + 1);
    }
  }
  const snapshot = Array.from(
    counts,
    ([app, count]) => count + "\t" + app,
  ).join("\n");
  const report = JSON.stringify([generation, snapshot]);
  if (report === lastSnapshot) return;
  callDBus(
    SERVICE,
    PATH,
    IFACE,
    "HiddenWindows",
    String(generation),
    String(++inventorySequence),
    snapshot,
    function (accepted) {
      if (
        accepted === true ||
        (Array.isArray(accepted) && accepted[0] === true)
      )
        lastSnapshot = report;
    },
  );
}

function appOf(w) {
  const known = (id) => id && (PROTECTED.includes(id) || RESTORE.includes(id));
  const visited = new Set();
  while (w && !visited.has(w)) {
    visited.add(w);
    if (known(w.desktopFileName)) return w.desktopFileName;
    if (known(w.resourceClass)) return w.resourceClass;
    const parent = w.transientFor;
    // A separately identified process (for example a portal) can present a
    // dialog for another app without belonging to that app.
    if (
      parent &&
      w.pid > 0 &&
      parent.pid > 0 &&
      w.pid !== parent.pid &&
      (w.desktopFileName || w.resourceClass)
    )
      break;
    w = parent;
  }
  return "";
}

function isAllowed(app) {
  return ALLOWED.indexOf(app) >= 0;
}

function locked(w) {
  const app = appOf(w);
  return PROTECTED.includes(app) && !isAllowed(app);
}

function isConcealed(w) {
  return (
    w.opacity === 0 &&
    w.minimized &&
    w.skipTaskbar &&
    w.skipSwitcher &&
    w.skipPager
  );
}

function capture(w) {
  const state = {};
  for (const key of Object.keys(concealedState)) state[key] = w[key];
  return state;
}

function applyState(w, state) {
  for (const key of Object.keys(concealedState)) {
    if (w[key] !== state[key]) w[key] = state[key];
  }
}

// A reply can get lost on agent restart. Only the latest write may finish
// this reconciliation, and the agent rejects retired generations.
function save(next) {
  if (!pendingSave) {
    saveStarted = Date.now();
    saveAttempts = 0;
  }
  saveAttempts++;
  if (
    saveAttempts >= SAVE_ATTEMPTS ||
    Date.now() - saveStarted >= SAVE_RETRY_MS * SAVE_ATTEMPTS
  ) {
    // The write is not coming back. Conceal with the captured values still
    // in memory so the lock still works. A later generation has no file and
    // falls back to the legacy defaults.
    originals = next;
    pendingSave = undefined;
    saveAttempts = 0;
    saveStarted = 0;
    reconcile("");
    return;
  }
  const serial = ++sequence;
  pendingSave = next;
  retryAt = Date.now() + SAVE_RETRY_MS;
  callDBus(
    SERVICE,
    PATH,
    IFACE,
    "SaveWindowSnapshots",
    String(generation),
    String(serial),
    JSON.stringify(Array.from(next)),
    function (ok) {
      if (sequence !== serial || !pendingSave) return;
      // KWin may hand the reply over as the value or as one-element args.
      const accepted = ok === true || (Array.isArray(ok) && ok[0] === true);
      if (accepted) {
        originals = next;
        pendingSave = undefined;
        saveAttempts = 0;
        saveStarted = 0;
        reconcile("");
      }
    },
  );
}

function reconcile(foreground) {
  if (foreground) requestedForeground = foreground;
  reportClosedApps();
  if (pendingSave) {
    // Retry the same snapshot even if a window closed during the call: the
    // previous write might already have reached disk and lost its reply.
    if (Date.now() >= retryAt) save(pendingSave);
    return;
  }
  restorePending = false;
  const windows = Array.from(workspace.windowList());
  const captured = new Map(originals);
  for (const w of windows) {
    const id = String(w.internalId);
    if (captured.has(id)) continue;
    // A window with the complete concealment signature and a known app but
    // no journal entry may be ours from a write that never reached disk.
    if (recoverUnknown && appOf(w) && isConcealed(w)) {
      captured.set(id, legacyOriginal);
    } else if (locked(w) && !isConcealed(w)) {
      captured.set(id, capture(w));
    }
  }
  if (captured.size !== originals.size) {
    save(captured);
    return;
  }
  recoverUnknown = false;
  const remaining = new Map();
  for (const w of windows) {
    const id = String(w.internalId);
    const original = originals.get(id);
    if (!original) continue;
    if (locked(w)) {
      remaining.set(id, original);
      applyState(w, concealedState);
    } else {
      applyState(w, original);
    }
  }
  // Restoring a parent can also change a transient. Verify the batch after
  // every setter has run, before forgetting any original values.
  let restoreFailed = false;
  for (const w of windows) {
    const id = String(w.internalId);
    const original = originals.get(id);
    if (
      original &&
      !locked(w) &&
      Object.keys(original).some((key) => w[key] !== original[key])
    ) {
      remaining.set(id, original);
      restoreFailed = true;
    }
  }
  if (remaining.size !== originals.size) {
    save(remaining);
    return;
  }
  if (restoreFailed) {
    if (restoreAttempts === 0) restoreStarted = Date.now();
    restoreAttempts++;
    if (Date.now() - restoreStarted < RESTORE_MAX_MS) {
      restorePending = true;
      restoreRetryAt = Date.now() + RESTORE_RETRY_MS;
    } else {
      restoreAttempts = 0;
    }
    return;
  }
  restoreAttempts = 0;
  foreground = requestedForeground;
  requestedForeground = "";
  if (
    foreground &&
    (ALLOWED.indexOf(foreground) >= 0 || PROTECTED.indexOf(foreground) < 0)
  ) {
    const window = Array.from(workspace.windowList()).find(
      (window) =>
        (window.desktopFileName === foreground ||
          window.resourceClass === foreground ||
          appOf(window) === foreground) &&
        (!window.minimized || !PROTECTED.includes(foreground)),
    );
    if (window) {
      window.desktops = [workspace.currentDesktop];
      window.minimized = false;
      workspace.raiseWindow(window);
      workspace.activeWindow = window;
    }
  }
  const targets = windows.filter(locked);
  const concealed = targets.filter(isConcealed);
  // Apps with a window the script could not conceal. The agent names them in
  // a lock result, so the user learns which app stayed visible instead of
  // only that the total did not match.
  const missed = new Map();
  for (const window of targets) {
    if (isConcealed(window)) continue;
    const app = appOf(window);
    // The snapshot is line-based, so an id that would split a line must not
    // reach it. reportHiddenWindows applies the same guard.
    if (app && !/[\x00\r\n]/.test(app))
      missed.set(app, (missed.get(app) || 0) + 1);
  }
  const missedSnapshot = Array.from(
    missed,
    ([app, count]) => count + "\t" + app,
  ).join("\n");
  callDBus(
    SERVICE,
    PATH,
    IFACE,
    "WindowsChecked",
    String(generation),
    String(targets.length),
    String(concealed.length),
    missedSnapshot,
    function () {},
  );
  reportHiddenWindows();
}
function watchIdentity(w) {
  // A dialog can acquire its parent or app id after windowAdded. Its own
  // children inherit that change as well. KWin exposes these as properties
  // whose changed signal may be absent or not connectable, so check the
  // signal before using it. `hiddenChanged` also covers a window that the
  // app hides without destroying it, which the close report needs to see.
  const changed = () => reconcile("");
  for (const name of [
    "transientChanged",
    "desktopFileNameChanged",
    "windowClassChanged",
    "hiddenChanged",
  ]) {
    const signal = w[name];
    try {
      if (signal && typeof signal.connect === "function")
        signal.connect(changed);
    } catch (error) {
      // This KWin version exposes the property without a connectable change
      // signal. windowAdded and activation still re-check the window.
    }
  }
}
for (const w of workspace.windowList()) watchIdentity(w);
workspace.windowAdded.connect(function (w) {
  watchIdentity(w);
  reconcile("");
});
if (workspace.windowRemoved)
  workspace.windowRemoved.connect(() => reconcile(""));

// The app that had focus before the current one. Losing focus starts its
// relock clock. Gaining it again cancels the clock, so switching back before
// the delay keeps the window visible. Recovering a window that was already
// hidden is the tray's or a relaunch's job.
let activeApp = "";
let activePid = "";
function report(app, pid, event) {
  if (!app) return;
  const key = app + "\t" + pid;
  const sequence = ++reportSequence;
  pendingReports.set(key, { app, pid, event, sequence });
  sendReport(key);
}
function sendReport(key) {
  const report = pendingReports.get(key);
  if (!report) return;
  callDBus(
    SERVICE,
    PATH,
    IFACE,
    "WindowEvent",
    report.app,
    report.pid,
    report.event,
    function (accepted) {
      const current = pendingReports.get(key);
      if (current && current.sequence === report.sequence && accepted !== false)
        pendingReports.delete(key);
    },
  );
}
function flushReports() {
  for (const key of pendingReports.keys()) sendReport(key);
}
function syncFocus(w) {
  const owner = appOf(w);
  const app = PROTECTED.includes(owner) ? owner : "";
  if (app === activeApp) return;
  report(activeApp, activePid, "unfocused");
  report(app, app ? "" + w.pid : "", "focused");
  activeApp = app;
  activePid = app ? "" + w.pid : "";
}
syncFocus(workspace.activeWindow);
workspace.windowActivated.connect(function (w) {
  const app = appOf(w);
  if (app && PROTECTED.includes(app) && !isAllowed(app)) {
    if (originals.has(String(w.internalId))) applyState(w, concealedState);
    else reconcile("");
    // A click on the app's own tray icon activates its hidden window. Keep
    // it hidden and ask the agent to restore it only if a ticket still
    // covers the app, so this path never lets the window show before the
    // ticket is confirmed.
    callDBus(SERVICE, PATH, IFACE, "RestoreIfTicketed", app, function () {});
    return;
  }
  if (w) syncFocus(w);
});

// Tell the agent this generation is running. It waits for this before
// trusting the load, because `run` can reach KWin before the source is
// compiled.
reconcile(/* @FOREGROUND@ */ "");
const timer = new QTimer();
timer.interval = 250;
timer.timeout.connect(function () {
  callDBus(SERVICE, PATH, IFACE, "WindowState", function (payload) {
    if (!payload) return;
    const [
      owner,
      revision,
      protectedApps,
      allowedApps,
      restoreApps,
      foreground,
    ] = JSON.parse(payload);
    if (owner !== instance) return;
    if (revision <= generation) {
      if (pendingSave && Date.now() >= retryAt) reconcile("");
      if (restorePending && Date.now() >= restoreRetryAt) reconcile("");
      flushReports();
      syncFocus(workspace.activeWindow);
      reportHiddenWindows();
      return;
    }
    requestedForeground = foreground;
    generation = revision;
    PROTECTED = protectedApps;
    ALLOWED = allowedApps;
    RESTORE = restoreApps;
    syncFocus(workspace.activeWindow);
    reconcile(foreground);
  });
});
timer.start();
