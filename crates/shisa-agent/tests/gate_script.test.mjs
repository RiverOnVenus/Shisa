import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";

// Run the same script shisa-agent sends to KWin, with a fake workspace and
// bus. The Rust tests check that the lists and names reach the script. These
// check what the script does with them: matching, concealing, restoring, and
// focus reporting.
const template = readFileSync(new URL("../src/gate-script.js", import.meta.url), "utf8");

function render({ locked = [], allowed = [], restore = [], generation = 1, foreground = "" }) {
  const script = template
    .replace('"@SERVICE@"', JSON.stringify("svc"))
    .replace('"@PATH@"', JSON.stringify("/window"))
    .replace('"@IFACE@"', JSON.stringify("iface"))
    .replace("/* @PROTECTED@ */ []", JSON.stringify(locked))
    .replace("/* @ALLOWED@ */ []", JSON.stringify(allowed))
    .replace("/* @RESTORE@ */ []", JSON.stringify(restore))
    .replace("/* @GENERATION@ */ 0", String(generation))
    .replace('/* @FOREGROUND@ */ ""', JSON.stringify(foreground));
  assert.ok(!script.includes("@"), "the template has an unreplaced placeholder");
  return script;
}

function makeWindow(desktopFileName, resourceClass, pid = 100) {
  return {
    desktopFileName,
    resourceClass,
    pid,
    opacity: 1,
    minimized: false,
    skipTaskbar: false,
    skipSwitcher: false,
    skipPager: false,
    transientFor: null,
  };
}

/** Load the script with fakes. `state` is the payload the next WindowState
 *  poll answers with, or undefined to leave the poll unanswered. */
function boot(options = {}) {
  const calls = [];
  const windows = options.windows ?? [];
  const handlers = { added: [], removed: [], activated: [] };
  const timers = [];

  class QTimer {
    constructor() {
      this.interval = 0;
      this.timeout = { connect: (fn) => (this.onTimeout = fn) };
      this.started = false;
      timers.push(this);
    }
    start() {
      this.started = true;
    }
  }

  const workspace = {
    currentDesktop: { id: "desk" },
    activeWindow: options.active ?? null,
    windowList: () => [...windows],
    windowAdded: { connect: (fn) => handlers.added.push(fn) },
    windowRemoved: { connect: (fn) => handlers.removed.push(fn) },
    windowActivated: { connect: (fn) => handlers.activated.push(fn) },
    raiseWindow: (window) => calls.push(["raise", window]),
  };

  function callDBus(...args) {
    calls.push(args);
    if (args[3] === "WindowState" && options.state !== undefined) {
      args[args.length - 1](JSON.stringify(options.state));
    }
  }

  vm.runInContext(render(options), vm.createContext({ workspace, callDBus, QTimer }));
  return { calls, windows, handlers, timers, workspace };
}

function methodCalls(env, method) {
  return env.calls.filter((call) => call[3] === method);
}

test("a locked app's window is concealed in every surface", () => {
  const window = makeWindow("org.kde.kate", "kate");
  const env = boot({ locked: ["org.kde.kate"], windows: [window] });
  assert.equal(window.opacity, 0);
  assert.equal(window.minimized, true);
  assert.equal(window.skipTaskbar, true);
  assert.equal(window.skipSwitcher, true);
  assert.equal(window.skipPager, true);

  const checked = methodCalls(env, "WindowsChecked");
  assert.equal(checked.length, 1);
  assert.deepEqual(checked[0].slice(4, 7), ["1", "1", "1"]);
  assert.equal(checked[0][7], "", "a fully concealed window has no missed app");

  const hidden = methodCalls(env, "HiddenWindows");
  assert.equal(hidden.length, 1);
  assert.equal(hidden[0][6], "1\torg.kde.kate");
});

test("a window the script could not conceal is named in the report", () => {
  const window = makeWindow("org.kde.kate", "kate");
  // KWin accepted the hide() calls but did not give this window the switcher
  // flag, so it is not concealed even though it looks hidden.
  Object.defineProperty(window, "skipSwitcher", {
    get: () => false,
    set: () => {},
  });
  const env = boot({ locked: ["org.kde.kate"], windows: [window] });
  const checked = methodCalls(env, "WindowsChecked");
  assert.deepEqual(checked[0].slice(4, 8), ["1", "1", "0", "1\torg.kde.kate"]);
});

test("an app id that would split the report line is left out", () => {
  const window = makeWindow("a\nb", "a\nb");
  Object.defineProperty(window, "skipSwitcher", {
    get: () => false,
    set: () => {},
  });
  const env = boot({ locked: ["a\nb"], windows: [window] });
  const checked = methodCalls(env, "WindowsChecked");
  assert.deepEqual(checked[0].slice(4, 8), ["1", "1", "0", ""]);
});

test("an allowed app is left visible", () => {
  const window = makeWindow("org.kde.kate", "kate");
  boot({ locked: ["org.kde.kate"], allowed: ["org.kde.kate"], windows: [window] });
  assert.equal(window.opacity, 1);
  assert.equal(window.minimized, false);
  assert.equal(window.skipTaskbar, false);
});

test("resourceClass matches when desktopFileName does not", () => {
  const window = makeWindow("something.else", "org.kde.kate");
  boot({ locked: ["org.kde.kate"], windows: [window] });
  assert.equal(window.opacity, 0);
  assert.equal(window.minimized, true);
});

test("an app id with a quote still runs", () => {
  const window = makeWindow('a"b', 'a"b');
  boot({ locked: ['a"b'], windows: [window] });
  assert.equal(window.opacity, 0);
});

test("a window hidden by an earlier generation is restored", () => {
  const window = makeWindow("old.app", "old");
  window.opacity = 0;
  window.minimized = true;
  window.skipTaskbar = true;
  window.skipSwitcher = true;
  window.skipPager = true;
  boot({ restore: ["old.app"], windows: [window] });
  assert.equal(window.opacity, 1);
  assert.equal(window.minimized, false);
  assert.equal(window.skipTaskbar, false);
});

test("a window the user minimized by hand stays minimized", () => {
  const window = makeWindow("unrelated.app", "unrelated");
  window.minimized = true;
  boot({ windows: [window] });
  assert.equal(window.opacity, 1);
  assert.equal(window.minimized, true);
});

test("a new window of a locked app is concealed", () => {
  const env = boot({ locked: ["org.kde.kate"], windows: [] });
  const window = makeWindow("org.kde.kate", "kate");
  env.windows.push(window);
  env.handlers.added[0](window);
  assert.equal(window.opacity, 0);
  assert.equal(window.minimized, true);
});

test("focus loss and gain are reported for protected apps", () => {
  const kate = makeWindow("org.kde.kate", "kate", 100);
  const terminal = makeWindow("org.kde.konsole", "konsole", 200);
  const env = boot({
    locked: ["org.kde.kate"],
    windows: [kate, terminal],
    active: kate,
  });
  const focused = methodCalls(env, "WindowEvent").filter((call) => call[6] === "focused");
  assert.deepEqual(focused[0].slice(4, 7), ["org.kde.kate", "100", "focused"]);

  env.workspace.activeWindow = terminal;
  env.handlers.activated[0](terminal);
  const unfocused = methodCalls(env, "WindowEvent").filter((call) => call[6] === "unfocused");
  assert.deepEqual(unfocused[0].slice(4, 7), ["org.kde.kate", "100", "unfocused"]);
});

test("activating a locked window hides it instead of reporting focus", () => {
  const kate = makeWindow("org.kde.kate", "kate");
  const env = boot({ locked: ["org.kde.kate"], windows: [kate] });
  const before = methodCalls(env, "WindowEvent").length;
  env.workspace.activeWindow = kate;
  env.handlers.activated[0](kate);
  assert.equal(kate.opacity, 0);
  assert.equal(kate.minimized, true);
  assert.equal(methodCalls(env, "WindowEvent").length, before);
  // An app's own tray icon activates its hidden window. The script keeps it
  // hidden and leaves the ticket check to the agent, so the window cannot
  // appear before that check.
  const restore = methodCalls(env, "RestoreIfTicketed");
  assert.equal(restore.length, 1);
  assert.equal(restore[0][4], "org.kde.kate");
});

test("activating an allowed window asks for no restore", () => {
  const kate = makeWindow("org.kde.kate", "kate");
  const env = boot({
    locked: ["org.kde.kate"],
    allowed: ["org.kde.kate"],
    windows: [kate],
  });
  env.workspace.activeWindow = kate;
  env.handlers.activated[0](kate);
  assert.equal(kate.opacity, 1);
  assert.equal(kate.minimized, false);
  assert.equal(methodCalls(env, "RestoreIfTicketed").length, 0);
});

test("a state revision applies a new protection list", () => {
  const kate = makeWindow("org.kde.kate", "kate");
  const env = boot({
    windows: [kate],
    state: [1, 2, ["org.kde.kate"], [], [], ""],
  });
  assert.equal(kate.opacity, 1);
  env.timers[0].onTimeout();
  assert.equal(kate.opacity, 0);
  assert.equal(kate.minimized, true);
  const checked = methodCalls(env, "WindowsChecked");
  assert.equal(checked.at(-1)[4], "2");
});

test("a foreground app is raised and activated", () => {
  const kate = makeWindow("org.kde.kate", "kate");
  const env = boot({
    locked: ["org.kde.kate"],
    windows: [kate],
    state: [1, 2, ["org.kde.kate"], ["org.kde.kate"], [], "org.kde.kate"],
  });
  assert.equal(kate.minimized, true);
  env.timers[0].onTimeout();
  assert.equal(kate.minimized, false);
  assert.equal(kate.opacity, 1);
  assert.equal(env.workspace.activeWindow, kate);
  assert.ok(env.calls.some((call) => call[0] === "raise" && call[1] === kate));
});

test("a transient follows its protected ancestor for concealment and inventory", () => {
  const parent = makeWindow("app", "app");
  const child = makeWindow("", "");
  const dialog = makeWindow("", "");
  dialog.transientFor = child;
  child.transientFor = parent;
  const env = boot({ locked: ["app"], windows: [parent, child, dialog] });
  assert.ok(dialog.minimized);
  assert.deepEqual(methodCalls(env, "WindowsChecked")[0].slice(5, 7), ["3", "3"]);
  assert.equal(methodCalls(env, "HiddenWindows")[0][6], "3\tapp");
  env.handlers.activated[0](dialog);
  assert.equal(methodCalls(env, "RestoreIfTicketed").at(-1)[4], "app");
});

test("a separately identified application keeps its own window policy", () => {
  const parent = makeWindow("app", "app");
  const other = makeWindow("other.app", "other", 200);
  other.transientFor = parent;
  boot({ locked: ["app", "other.app"], allowed: ["other.app"], windows: [parent, other] });
  assert.equal(other.minimized, false);
  boot({ locked: ["app"], windows: [parent, other] });
  assert.equal(other.minimized, false);
});


test("a newly assigned transient parent updates the window and its descendants", () => {
  const parent = makeWindow("app", "app");
  const child = makeWindow("", "");
  let changed;
  child.transientChanged = { connect(fn) { changed = fn; } };
  const grandchild = makeWindow("", "");
  grandchild.transientFor = child;
  boot({ locked: ["app"], windows: [parent, child, grandchild] });
  assert.equal(grandchild.minimized, false);
  child.transientFor = parent;
  changed();
  assert.equal(grandchild.minimized, true);
});
