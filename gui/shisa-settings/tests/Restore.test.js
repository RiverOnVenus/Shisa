// The restore panel against a fake Tauri shell. It shows the app id only when
// two hidden apps share a display name, because the name alone would not tell
// the rows apart.

import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { mount, unmount } from "svelte";

const { state } = vi.hoisted(() => ({
  state: { installed: [], hidden: {} },
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (command) => {
    switch (command) {
      case "hidden_apps":
        return { ...state.hidden };
      case "list_installed":
        return state.installed;
      case "list_apps":
        return state.installed.map(([id]) => [id, false, [], []]);
      case "agent_status":
        return true;
      case "color_scheme":
        return null;
      case "icon":
        return "";
      default:
        return null;
    }
  }),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    close: vi.fn(async () => {}),
    startDragging: vi.fn(),
  }),
}));

import Restore from "../src/Restore.svelte";

let app;

beforeEach(() => {
  document.body.innerHTML = "";
  document.documentElement.lang = "";
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
