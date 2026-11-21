import { mount } from "svelte";
import { invoke } from "@tauri-apps/api/core";
import "./app.css";
import App from "./App.svelte";
import Restore from "./Restore.svelte";
import { getCurrentWindow } from "@tauri-apps/api/window";

// The first paint's theme: the desktop's own preference as GTK reports it.
// The portal's answer replaces this once the window asks, because KDE keeps
// GTK light under a dark Plasma (see app.css and main.rs).
document.documentElement.dataset.theme = matchMedia(
  "(prefers-color-scheme: dark)",
).matches
  ? "dark"
  : "light";

// Ask the shell for the saved language before mounting, so an English window
// never paints Chinese first. The label picks the window; both share the
// index.html entry.
const window = getCurrentWindow();
invoke("ui_language")
  .catch(() => navigator.language)
  .then((language) => {
    document.documentElement.lang = language;
    mount(window.label === "restore" ? Restore : App, {
      target: document.getElementById("app"),
      props: { language },
    });
  });
