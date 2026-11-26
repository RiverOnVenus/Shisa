import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";

// Run the same script shisa-agent sends to KWin, with a fake workspace and
// bus. The Rust tests check that the lists and names reach the script. These
// check what the script does with them: matching, concealing, restoring, and
// focus reporting.
const template = readFileSync(new URL("../src/gate-script.js", import.meta.url), "utf8");

function render({ locked = [], allowed = [], restore = [], generation = 1, foreground = "", journal = { value: "[]" } }) {
  const script = template
    .replace('"@SERVICE@"', JSON.stringify("svc"))
    .replace('/* @SNAPSHOTS@ */ "[]"', JSON.stringify(journal.value))
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

let nextId = 0;
function makeWindow(desktopFileName, resourceClass, pid = 100) {
  return {
    internalId: String(++nextId),
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
    if (args[3] === "SaveWindowSnapshots") {
      if (options.failSave) {
        args.at(-1)(false);
        return;
      }
      const commit = () => {
        if (options.journal) options.journal.value = args[6];
        args.at(-1)(true);
      };
      if (options.defer) options.defer.push(commit);
      else commit();
    }
    if (args[3] === "WindowState") {
      const owner = options.generation ?? 1;
      const payload = options.state ?? [owner, 1, [], [], [], ""];
      args[args.length - 1](JSON.stringify(payload));
    }
  }

  vm.runInContext(
    render(options),
    vm.createContext({ workspace, callDBus, QTimer, Date: options.Date ?? Date }),
  );
  return { calls, windows, handlers, timers, workspace, options };
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
  window.opacity = 0.75;
  window.skipPager = true;
  window.minimized = true;
  const journal = { value: "[]" };
  boot({ locked: ["old.app"], windows: [window], journal });
  assert.equal(window.opacity, 0);
  boot({ restore: ["old.app"], windows: [window], journal, generation: 2 });
  assert.equal(window.opacity, 0.75);
  assert.equal(window.minimized, true);
  assert.equal(window.skipTaskbar, false);
  assert.equal(window.skipPager, true);
  assert.equal(journal.value, "[]");
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


test("a hidden-looking unrelated window keeps all its properties", () => {
  const w = makeWindow("other", "other");
  Object.assign(w, { opacity: 0, minimized: true, skipTaskbar: true, skipPager: true, skipSwitcher: true });
  const before = { ...w };
  boot({ locked: ["app"], windows: [w] });
  assert.deepEqual(w, before);
});

test("a closed window cannot pass its snapshot to a new window of the same app", () => {
  const old = makeWindow("app", "app");
  old.opacity = 0.4;
  const defer = [];
  const journal = { value: "[]" };
  const env = boot({ locked: ["app"], windows: [old], journal, defer });
  assert.equal(old.opacity, 0.4);
  assert.equal(methodCalls(env, "WindowsChecked").length, 0);
  env.windows.pop();
  env.handlers.removed[0](old);
  const next = makeWindow("app", "app");
  env.windows.push(next);
  env.handlers.added[0](next);
  while (defer.length) defer.shift()();
  assert.equal(next.opacity, 0);
  assert.deepEqual(JSON.parse(journal.value).map(([id]) => id), [next.internalId]);
  boot({ windows: [next], journal, generation: 2 });
  assert.equal(next.opacity, 1);
  assert.equal(journal.value, "[]");
});

test("explicit restore activates a visible window and preserves its minimized sibling", () => {
  const minimized = makeWindow("app", "app");
  minimized.minimized = true;
  const visible = makeWindow("app", "app");
  const env = boot({ locked: ["app"], windows: [minimized, visible],
    state: [1, 2, ["app"], ["app"], [], "app"] });
  env.timers[0].onTimeout();
  assert.equal(minimized.minimized, true);
  assert.equal(env.workspace.activeWindow, visible);
});

test("a concealed known window missing from the journal is restored, an unrelated one is not", () => {
  const app = makeWindow("app", "app");
  const unrelated = makeWindow("unrelated", "unrelated");
  for (const w of [app, unrelated]) {
    Object.assign(w, { minimized: true, opacity: 0, skipTaskbar: true, skipPager: true, skipSwitcher: true });
  }
  boot({ locked: ["app"], allowed: ["app"], windows: [app, unrelated] });
  assert.equal(app.opacity, 1);
  assert.equal(unrelated.opacity, 0);
});

test("a corrupt snapshot journal starts empty instead of failing the script", () => {
  const window = makeWindow("app", "app");
  const journal = { value: "{" };
  boot({ locked: ["app"], windows: [window], journal });
  assert.equal(window.opacity, 0);
  assert.equal(window.minimized, true);
  const written = JSON.parse(journal.value);
  assert.ok(Array.isArray(written));
  assert.equal(written.length, 1);
});

test("a snapshot with the wrong shape is ignored", () => {
  const window = makeWindow("app", "app");
  const journal = {
    value: JSON.stringify([[String(window.internalId), { opacity: "zero" }]]),
  };
  boot({ locked: ["app"], windows: [window], journal });
  assert.equal(window.opacity, 0);
  const written = JSON.parse(journal.value);
  assert.equal(written.length, 1);
  assert.equal(written[0][1].opacity, 1);
});

test("a snapshot that cannot be saved still conceals after a bounded retry", () => {
  let now = 0;
  class ClockDate extends Date {
    static now() {
      return now;
    }
  }
  const window = makeWindow("app", "app");
  window.opacity = 0.4;
  const journal = { value: "[]" };
  const env = boot({
    locked: ["app"],
    windows: [window],
    journal,
    failSave: true,
    Date: ClockDate,
  });
  assert.equal(window.opacity, 0.4);
  // Two retries, then the third save attempt gives up and conceals with the
  // captured original still in memory.
  for (let i = 0; i < 3; i++) {
    now += 500;
    env.timers[0].onTimeout();
  }
  assert.equal(window.opacity, 0);
  assert.equal(window.minimized, true);
  assert.equal(journal.value, "[]");

  // Unlocking in the same generation restores the captured values.
  env.options.state = [1, 2, ["app"], ["app"], [], ""];
  now += 500;
  env.timers[0].onTimeout();
  assert.equal(window.opacity, 0.4);
  assert.equal(window.minimized, false);
});

test("a corrupt journal migrates a concealed known window back", () => {
  const window = makeWindow("app", "app");
  Object.assign(window, {
    opacity: 0,
    minimized: true,
    skipTaskbar: true,
    skipSwitcher: true,
    skipPager: true,
  });
  const journal = { value: "{" };
  boot({
    locked: ["app"],
    allowed: ["app"],
    windows: [window],
    journal,
  });
  assert.equal(window.opacity, 1);
  assert.equal(window.minimized, false);
});

test("a concealed window missing from a valid journal is recovered when allowed", () => {
  let now = 0;
  class ClockDate extends Date {
    static now() {
      return now;
    }
  }
  const window = makeWindow("app", "app");
  window.opacity = 0.4;
  const journal = { value: "[]" };
  const env = boot({
    locked: ["app"],
    windows: [window],
    journal,
    failSave: true,
    Date: ClockDate,
  });
  // The save cannot complete, so the fallback conceals with the values in
  // memory while the older empty journal stays on disk.
  for (let i = 0; i < 3; i++) {
    now += 500;
    env.timers[0].onTimeout();
  }
  assert.equal(window.opacity, 0);
  assert.equal(window.minimized, true);
  assert.equal(journal.value, "[]");

  // A fresh generation reads the valid empty journal and finds a concealed
  // known window. It recovers the window with the defaults.
  boot({
    locked: ["app"],
    allowed: ["app"],
    windows: [window],
    journal,
    generation: 2,
  });
  assert.equal(window.opacity, 1);
  assert.equal(window.minimized, false);
  assert.equal(journal.value, "[]");
});

test("a concealed window missing from a valid journal stays hidden while locked", () => {
  const window = makeWindow("app", "app");
  Object.assign(window, {
    opacity: 0,
    minimized: true,
    skipTaskbar: true,
    skipSwitcher: true,
    skipPager: true,
  });
  const journal = { value: "[]" };
  const env = boot({
    locked: ["app"],
    windows: [window],
    journal,
    generation: 2,
  });
  // The recovery records the defaults, but the app is still locked.
  assert.equal(window.opacity, 0);
  assert.equal(window.minimized, true);
  assert.notEqual(journal.value, "[]");

  env.options.state = [2, 3, ["app"], ["app"], [], ""];
  env.timers[0].onTimeout();
  assert.equal(window.opacity, 1);
  assert.equal(window.minimized, false);
  assert.equal(journal.value, "[]");
});

test("a restore that KWin delays is retried", () => {
  let now = 0;
  class ClockDate extends Date {
    static now() {
      return now;
    }
  }
  const window = makeWindow("app", "app");
  let minimized = false;
  let blockRestore = true;
  Object.defineProperty(window, "minimized", {
    get: () => minimized,
    set: (value) => {
      if (value === false && blockRestore) return;
      minimized = value;
    },
  });
  const env = boot({
    locked: ["app"],
    windows: [window],
    journal: { value: "[]" },
    Date: ClockDate,
  });
  assert.equal(window.opacity, 0);
  assert.equal(minimized, true);

  // Unlock: the first restore is blocked, so the script keeps the original
  // and schedules a retry.
  env.options.state = [1, 2, ["app"], ["app"], [], ""];
  now += 250;
  env.timers[0].onTimeout();
  assert.equal(minimized, true);

  blockRestore = false;
  now += 500;
  env.timers[0].onTimeout();
  assert.equal(minimized, false);
});
