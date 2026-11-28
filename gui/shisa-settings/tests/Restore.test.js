// The restore panel against a fake Tauri shell. It shows the app id only when
// two hidden apps share a display name, because the name alone would not tell
// the rows apart.

import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { mount, unmount } from "svelte";

const { state, close } = vi.hoisted(() => ({
  state: { installed: [], hidden: {}, schemeRead: null, listeners: [] },
  close: vi.fn(async () => {}),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (command, args) => {
    switch (command) {
      case "lock":
        return {
          summary: "Lock applied. No protected window matched on this desktop",
          waitForInventory: false,
        };
      case "hidden_apps":
        return { ...state.hidden };
      case "restore_hidden":
        delete state.hidden[args.appId];
        return null;
      case "list_installed":
        return state.installed;
      case "list_apps":
        return state.installed.map(([id]) => [id, false, [], []]);
      case "agent_status":
        return true;
      case "color_scheme":
        return state.schemeRead;
      case "icon":
        return "";
      default:
        return null;
    }
  }),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name, handler) => {
    state.listeners.push({ name, handler });
    return () => {};
  }),
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    close,
    startDragging: vi.fn(),
  }),
}));

import Restore from "../src/Restore.svelte";

let app;

beforeEach(() => {
  document.body.innerHTML = "";
  document.documentElement.lang = "";
  document.documentElement.dataset.theme = "";
  state.schemeRead = null;
  state.listeners = [];
  close.mockClear();
});

afterEach(async () => {
  if (app) await unmount(app);
  app = null;
});

function rows() {
  return [...document.querySelectorAll(".row")].map((row) => ({
    name: row.querySelector(".name")?.textContent,
    id: row.querySelector(".id")?.textContent,
  }));
}

async function render() {
  app = mount(Restore, { target: document.body, props: { language: "en" } });
  await vi.waitFor(() =>
    expect(rows().length).toBe(Object.keys(state.hidden).length),
  );
}

test("a colour signal that beats the first read is not overwritten", async () => {
  let answer;
  state.schemeRead = new Promise((resolve) => {
    answer = resolve;
  });
  await render();
  await vi.waitFor(() => expect(state.listeners.length).toBeGreaterThan(0));
  const listener = state.listeners.find(
    (entry) => entry.name === "color_scheme",
  );
  listener.handler({ payload: true });
  answer(false);
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(document.documentElement.dataset.theme).toBe("dark");
});

test("two apps with one display name show their ids", async () => {
  state.installed = [
    ["org.example.editor", "Text Editor", ""],
    ["org.example.notes", "Text Editor", ""],
  ];
  state.hidden = { "org.example.editor": 1, "org.example.notes": 2 };
  await render();
  expect(rows()).toEqual([
    { name: "Text Editor", id: "org.example.editor" },
    { name: "Text Editor", id: "org.example.notes" },
  ]);
});

test("a unique display name needs no id", async () => {
  state.installed = [
    ["org.example.editor", "Text Editor", ""],
    ["org.example.notes", "Notes", ""],
  ];
  state.hidden = { "org.example.editor": 1, "org.example.notes": 2 };
  await render();
  expect(rows()).toEqual([
    { name: "Notes", id: undefined },
    { name: "Text Editor", id: undefined },
  ]);
});

test("restoring the last window confirms before the panel closes", async () => {
  state.installed = [["org.example.editor", "Text Editor", ""]];
  state.hidden = { "org.example.editor": 1 };
  await render();
  document.querySelector(".row").click();
  await vi.waitFor(() =>
    expect(document.body.textContent).toContain(
      "Restored. The window is coming back.",
    ),
  );
  // Enter was just used to restore. The safe focus target is the close
  // button, not the empty state's lock action.
  await vi.waitFor(() =>
    expect(document.activeElement?.getAttribute("aria-label")).toBe("Close"),
  );
  await vi.waitFor(() => expect(close).toHaveBeenCalled(), { timeout: 2000 });
});

test("the empty panel explains that only protected apps appear", async () => {
  state.installed = [["org.example.editor", "Text Editor", ""]];
  state.hidden = {};
  await render();
  await vi.waitFor(() =>
    expect(document.body.textContent).toContain(
      "Only protected apps appear here.",
    ),
  );
  const buttons = [...document.querySelectorAll(".blank-actions button")];
  expect(buttons.map((button) => button.textContent.trim())).toEqual([
    "Open settings",
    "Lock now",
  ]);
  expect(buttons[1].getAttribute("title")).toBe(
    "Only protected apps are locked. Unprotected apps are unaffected.",
  );
  // A stray Enter after the panel opens must not lock the desktop.
  await vi.waitFor(() =>
    expect(document.activeElement?.textContent.trim()).toBe("Open settings"),
  );
});

test("the empty panel hides the lock action when nothing is protected", async () => {
  state.installed = [];
  state.hidden = {};
  await render();
  await vi.waitFor(() =>
    expect(document.body.textContent).toContain("No apps are protected yet."),
  );
  expect(
    [...document.querySelectorAll(".blank-actions button")].map((button) =>
      button.textContent.trim(),
    ),
  ).toEqual(["Open settings"]);
  await vi.waitFor(() =>
    expect(document.activeElement?.textContent.trim()).toBe("Open settings"),
  );
});

test("the restore panel shows the shell's lock summary", async () => {
  state.installed = [["org.example.editor", "Text Editor", ""]];
  state.hidden = {};
  await render();
  const lockButton = () => document.querySelector(".blank-actions button:last-child");
  await vi.waitFor(() => expect(lockButton()?.textContent.trim()).toBe("Lock now"));
  lockButton().click();
  await vi.waitFor(() =>
    expect(document.body.textContent).toContain(
      "Lock applied. No protected window matched on this desktop",
    ),
  );
});

test("the top bar opens settings and closes the panel", async () => {
  state.installed = [["org.example.editor", "Text Editor", ""]];
  state.hidden = { "org.example.editor": 1 };
  await render();
  document.querySelector('button[aria-label="Settings"]').click();
  await vi.waitFor(() => expect(close).toHaveBeenCalled());
});
