import { mount } from "svelte";
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

export default mount(getCurrentWindow().label === "restore" ? Restore : App, {
  target: document.getElementById("app"),
});
