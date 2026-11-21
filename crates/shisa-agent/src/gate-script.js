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

let hidden = [];
let inventorySequence = 0;
let lastSnapshot;

function reportHiddenWindows() {
  const counts = new Map();
  for (const window of hidden) {
    const app = appOf(window);
    if (!app || /[\x00\r\n]/.test(app)) continue;
    if (looksHidden(window) && window.skipSwitcher && window.skipPager) {
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
    function () {
      lastSnapshot = report;
    },
  );
}

function appOf(w) {
  if (!w) return "";
  const known = function (id) {
    return id && (PROTECTED.indexOf(id) >= 0 || RESTORE.indexOf(id) >= 0);
  };
  if (known(w.desktopFileName)) return w.desktopFileName;
  if (known(w.resourceClass)) return w.resourceClass;
  return "";
}

function focusedApp(window) {
  const visited = new Set();
  while (window && !visited.has(window)) {
    visited.add(window);
    const app = appOf(window);
    if (app && PROTECTED.includes(app)) return app;
    window = window.transientFor;
  }
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
  if (!ours) return;
  if (i >= 0) hidden.splice(i, 1);
  w.opacity = 1;
  w.skipTaskbar = false;
  w.skipSwitcher = false;
  w.skipPager = false;
  w.minimized = false;
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

function reconcile(foreground) {
  for (const window of Array.from(workspace.windowList())) {
    if (appOf(window)) consider(window);
    else if (hidden.indexOf(window) >= 0 || looksHidden(window)) show(window);
  }
  if (
    foreground &&
    (ALLOWED.indexOf(foreground) >= 0 || PROTECTED.indexOf(foreground) < 0)
  ) {
    const window = Array.from(workspace.windowList()).find(
      (window) =>
        window.desktopFileName === foreground ||
        window.resourceClass === foreground,
    );
    if (window) {
      window.desktops = [workspace.currentDesktop];
      window.minimized = false;
      workspace.raiseWindow(window);
      workspace.activeWindow = window;
    }
  }
  const targets = Array.from(workspace.windowList()).filter((window) => {
    const app = appOf(window);
    return PROTECTED.indexOf(app) >= 0 && !isAllowed(app);
  });
  const concealed = targets.filter(
    (window) => looksHidden(window) && window.skipSwitcher && window.skipPager,
  );
  if (
    foreground &&
    ALLOWED.includes(foreground) &&
    Array.from(workspace.windowList()).some(
      (window) => appOf(window) === foreground && looksHidden(window),
    )
  ) {
    reportHiddenWindows();
    return;
  }
  callDBus(
    SERVICE,
    PATH,
    IFACE,
    "WindowsChecked",
    String(generation),
    String(targets.length),
    String(concealed.length),
    function () {},
  );
  reportHiddenWindows();
}
workspace.windowAdded.connect(function (w) {
  consider(w);
  if (hidden.indexOf(w) >= 0) reportHiddenWindows();
});
if (workspace.windowRemoved) {
  workspace.windowRemoved.connect(function (w) {
    const i = hidden.indexOf(w);
    if (i >= 0) {
      hidden.splice(i, 1);
      reportHiddenWindows();
    }
  });
}

// The app that had focus before the current one. Losing focus starts its
// relock clock. Gaining it again cancels the clock, so switching back before
// the delay keeps the window visible. Recovering a window that was already
// hidden is the tray's or a relaunch's job.
let activeApp = "";
let activePid = "";
function report(app, pid, event) {
  if (app)
    callDBus(
      SERVICE,
      PATH,
      IFACE,
      "WindowEvent",
      app,
      pid,
      event,
      function () {},
    );
}
function syncFocus(w) {
  const app = focusedApp(w);
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
    hide(w);
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
      syncFocus(workspace.activeWindow);
      reportHiddenWindows();
      return;
    }
    generation = revision;
    PROTECTED = protectedApps;
    ALLOWED = allowedApps;
    RESTORE = restoreApps;
    syncFocus(workspace.activeWindow);
    reconcile(foreground);
  });
});
timer.start();
