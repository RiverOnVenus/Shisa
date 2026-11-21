// The settings window against a fake Tauri shell. format.test.js covers the
// pure rules. These mount the real component and check that it renders what
// the daemon answered, filters it, and switches language.

import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { mount, unmount } from "svelte";

const { calls, languageWrites } = vi.hoisted(() => ({
  calls: [],
  languageWrites: [],
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (command, args) => {
    calls.push([command, args]);
    switch (command) {
      case "list_installed":
        return [
          ["org.kde.kate", "Kate", "kate"],
          ["org.kde.dolphin", "Dolphin", "dolphin"],
        ];
      case "list_apps":
        return [["org.kde.kate", true, ["/usr/bin/kate"], ["/usr/bin/kate"]]];
      case "app_config":
        return [300, false, true, 0];
      case "resolve_app":
        return [["/usr/bin/kate"], []];
      case "global_config":
        return [300, true, 60];
      case "recent_launches":
        return [[], 0, 0, 0];
      case "diagnostics":
        return ["0.1.1", 1, 0, 0, 1, 1, "/etc/shisa/users/test.toml", true];
      case "set_ui_language":
        languageWrites.push(args.language);
        return null;
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
    setTitle: vi.fn(async () => {}),
    startResizeDragging: vi.fn(),
    minimize: vi.fn(),
    close: vi.fn(),
  }),
}));

import App from "../src/App.svelte";

let app;

beforeEach(() => {
  calls.length = 0;
  languageWrites.length = 0;
  document.body.innerHTML = "";
  document.documentElement.lang = "";
});

afterEach(async () => {
  if (app) await unmount(app);
  app = null;
});

function rowNames() {
  return [...document.querySelectorAll("ul.list .row .name")].map(
    (name) => name.textContent,
  );
}

async function render() {
  app = mount(App, { target: document.body, props: { language: "en" } });
  await vi.waitFor(() => expect(rowNames().length).toBe(2));
  return app;
}

test("renders the installed apps and the daemon state", async () => {
  await render();
  expect(rowNames()).toEqual(["Kate", "Dolphin"]);
  expect(document.querySelector(".side-foot .gw").textContent).toContain(
    "App lock is running",
  );
  // The protected row is selected on mount, so its settings are loaded.
  expect(calls).toContainEqual(["app_config", { appId: "org.kde.kate" }]);
});

test("the search narrows the sidebar", async () => {
  await render();
  const search = document.querySelector('input[aria-label="Search apps"]');
  search.value = "dol";
  search.dispatchEvent(new Event("input", { bubbles: true }));
  await vi.waitFor(() => expect(rowNames()).toEqual(["Dolphin"]));
});

test("the protected filter leaves only protected apps", async () => {
  await render();
  const button = [...document.querySelectorAll(".side-scroll .segs .opt")].find(
    (option) => option.textContent === "Protected",
  );
  button.click();
  await vi.waitFor(() => expect(rowNames()).toEqual(["Kate"]));
});

test("selecting an app loads its settings", async () => {
  await render();
  const dolphin = [...document.querySelectorAll("ul.list .row")].find((row) =>
    row.textContent.includes("Dolphin"),
  );
  dolphin.click();
  await vi.waitFor(() =>
    expect(calls).toContainEqual(["app_config", { appId: "org.kde.dolphin" }]),
  );
  expect(document.querySelector(".crumb b").textContent).toBe("Dolphin");
});

test("choosing a language saves it and retitles the window", async () => {
  await render();
  document.querySelector(".side-foot button.row").click();
  await vi.waitFor(() =>
    expect(document.body.textContent).toContain("Default grace period"),
  );
  const chinese = [...document.querySelectorAll(".segs.two .opt")].find(
    (option) => option.textContent === "简体中文",
  );
  chinese.click();
  await vi.waitFor(() => {
    expect(languageWrites).toEqual(["zh-CN"]);
    expect(document.title).toBe("Shisa 设置");
  });
  expect(document.documentElement.lang).toBe("zh-CN");
});
