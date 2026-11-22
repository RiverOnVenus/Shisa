// The settings window against a fake Tauri shell. format.test.js covers the
// pure rules. These mount the real component and check that it renders what
// the daemon answered, filters it, and switches language.

import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { mount, unmount } from "svelte";

const { calls, languageWrites, state } = vi.hoisted(() => ({
  calls: [],
  languageWrites: [],
  state: {
    agentOnline: true,
    diagnostics: [
      "0.1.1",
      1,
      0,
      0,
      1,
      1,
      "/etc/shisa/users/test.toml",
      true,
      0,
    ],
  },
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
        return state.diagnostics;
      case "agent_status":
        return state.agentOnline;
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
  state.agentOnline = true;
  state.diagnostics = [
    "0.1.1",
    1,
    0,
    0,
    1,
    1,
    "/etc/shisa/users/test.toml",
    true,
    0,
  ];
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
  await vi.waitFor(() =>
    expect(document.querySelector(".side-foot .gw").textContent).toContain(
      "Running normally",
    ),
  );
  // The protected row is selected on mount, so its settings are loaded.
  expect(calls).toContainEqual(["app_config", { appId: "org.kde.kate" }]);
});

test("the health line names a missing session agent", async () => {
  state.agentOnline = false;
  await render();
  await vi.waitFor(() =>
    expect(document.querySelector(".side-foot .gw").textContent).toContain(
      "Session agent is not running",
    ),
  );
  expect(document.querySelector(".side-foot .gw.warn")).not.toBeNull();
});

test("the General row draws its own gear instead of asking the icon theme", async () => {
  await render();
  // `preferences-system` lives outside apps/ in several themes, and its
  // colours are the theme's rather than the row's.
  const general = document.querySelector(".side-foot button.row");
  expect(general.querySelector("svg")).not.toBeNull();
  expect(calls).not.toContainEqual(["icon", { name: "preferences-system" }]);
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

test("the settings search is case-insensitive and covers the language card", async () => {
  await render();
  document.querySelector(".side-foot button.row").click();
  await vi.waitFor(() =>
    expect(document.body.textContent).toContain("Default grace period"),
  );
  const cardWith = (text) =>
    [...document.querySelectorAll(".set")].find((card) =>
      card.textContent.includes(text),
    );
  const language = cardWith("Language");
  const grace = cardWith("Default grace period");
  const search = document.querySelector(".top-q");

  search.value = "LANGUAGE";
  search.dispatchEvent(new Event("input", { bubbles: true }));
  await vi.waitFor(() => expect(language.hidden).toBe(false));
  expect(grace.hidden).toBe(true);

  search.value = "zzz";
  search.dispatchEvent(new Event("input", { bubbles: true }));
  await vi.waitFor(() => expect(language.hidden).toBe(true));
  expect(document.querySelector(".set-none").hidden).toBe(false);
});

test("the diagnostics card is folded and the search opens the fold it matches", async () => {
  await render();
  document.querySelector(".side-foot button.row").click();
  await vi.waitFor(() =>
    expect(document.body.textContent).toContain("Default grace period"),
  );
  const card = [...document.querySelectorAll(".set")].find((section) =>
    section.textContent.includes("Diagnostics"),
  );
  const fold = card.querySelector("details");
  expect(fold).not.toBeNull();
  expect(fold.open).toBe(false);

  const search = document.querySelector(".top-q");
  search.value = "0.1.1";
  search.dispatchEvent(new Event("input", { bubbles: true }));
  await vi.waitFor(() => expect(fold.open).toBe(true));

  search.value = "";
  search.dispatchEvent(new Event("input", { bubbles: true }));
  await vi.waitFor(() => expect(fold.open).toBe(false));
});

test("a marking failure shows without opening the diagnostics fold", async () => {
  // The tuple is version, started, reloaded, failed, apps, marked, config,
  // password readiness and the configuration failure time.
  state.diagnostics = [
    "0.1.1",
    1,
    0,
    5,
    1,
    1,
    "/etc/shisa/users/test.toml",
    true,
    0,
  ];
  await render();
  document.querySelector(".side-foot button.row").click();
  await vi.waitFor(() =>
    expect(document.body.textContent).toContain(
      "Some paths could not be marked",
    ),
  );
  const card = [...document.querySelectorAll(".set")].find((section) =>
    section.textContent.includes("Diagnostics"),
  );
  expect(card.querySelector("details").open).toBe(false);
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
