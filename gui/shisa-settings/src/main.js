import { mount } from "svelte";
import "./app.css";
import App from "./App.svelte";

// The first paint's theme: the desktop's own preference as GTK reports it.
// The portal's answer replaces this once the window asks, because KDE keeps
// GTK light under a dark Plasma (see app.css and main.rs).
document.documentElement.dataset.theme = matchMedia(
  "(prefers-color-scheme: dark)",
).matches
  ? "dark"
  : "light";

export default mount(App, { target: document.getElementById("app") });
