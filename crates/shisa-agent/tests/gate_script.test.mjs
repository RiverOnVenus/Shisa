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

  const hidden = methodCalls(env, "HiddenWindows");
  assert.equal(hidden.length, 1);
  assert.equal(hidden[0][6], "1\torg.kde.kate");
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
  assert.equal(methodCalls(env, "WindowEvent").length, before);
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
