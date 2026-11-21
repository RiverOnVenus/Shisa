<script>
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { onMount, tick, untrack } from "svelte";
  import { createTranslator, languageFromTag } from "./i18n.js";

  let { language: initialLanguage = "en" } = $props();
  // The choice is fixed for the life of this window. It only reads it, so a
  // later change in the settings window waits for the next open.
  const lang = untrack(() => languageFromTag(initialLanguage));
  const t = createTranslator(lang);

  $effect(() => {
    document.title = t("restore.title");
  });

  const panel = getCurrentWindow();
  let rows = $state([]);
  let icons = $state({});
  let loading = $state(true);
  let opening = $state("");
  let error = $state("");
  let list;

  function theme(dark) {
    if (dark !== null) document.documentElement.dataset.theme = dark ? "dark" : "light";
  }

  onMount(() => {
    const unlisten = listen("color_scheme", (event) => theme(event.payload));
    invoke("color_scheme").then(theme).catch(() => {});
    load();
    return () => { unlisten.then((stop) => stop()).catch(() => {}); };
  });

  async function load() {
    try {
      const [counts, installed] = await Promise.all([
        invoke("hidden_apps"),
        invoke("list_installed").catch(() => []),
      ]);
      const apps = new Map(installed.map(([id, name, icon]) => [id, { name, icon }]));
      rows = Object.entries(counts)
        .filter(([, count]) => count > 0)
        .map(([id, count]) => ({ id, count, name: id, ...apps.get(id) }))
        .sort((left, right) => left.name.localeCompare(right.name));
      for (const row of rows) {
        if (row.icon && !(row.icon in icons)) {
          invoke("icon", { name: row.icon }).then((icon) => {
            icons[row.icon] = icon;
          }).catch(() => {});
        }
      }
    } catch {
      error = t("restore.error.read");
    } finally {
      loading = false;
      await tick();
      list?.querySelector("button")?.focus();
    }
  }

  async function restore(id) {
    if (opening) return;
    opening = id;
    error = "";
    try {
      await invoke("restore_hidden", { appId: id });
      await panel.close();
    } catch (reason) {
      error = typeof reason === "string" ? reason : t("restore.error.restore");
      opening = "";
    }
  }

  function keydown(event) {
    if (event.key === "Escape" && !opening) {
      panel.close();
    }
    if (opening || (event.key !== "ArrowDown" && event.key !== "ArrowUp")) return;
    const buttons = [...(list?.querySelectorAll("button") ?? [])];
    if (!buttons.length) return;
    event.preventDefault();
    const index = buttons.indexOf(document.activeElement);
    const next = index < 0 ? 0 : (index + (event.key === "ArrowDown" ? 1 : -1) + buttons.length) % buttons.length;
    buttons[next].focus();
  }
</script>

<svelte:window onkeydown={keydown} />

<main>
  <header>
    <h1>{t("restore.title")}</h1>
    {#if rows.length}<span>{t("restore.windows", { count: rows.reduce((total, row) => total + row.count, 0) })}</span>{/if}
  </header>
  <div class="list" bind:this={list} aria-busy={loading || !!opening}>
    {#if loading}
      <p class="empty" role="status">{t("restore.loading")}</p>
    {:else if !rows.length && !error}
      <p class="empty">{t("restore.empty")}</p>
    {:else}
      {#each rows as row (row.id)}
        <button type="button" disabled={!!opening} onclick={() => restore(row.id)}>
          <span class="app-icon" aria-hidden="true">
            {#if icons[row.icon]}
              <img src={icons[row.icon]} alt="" />
            {:else}
              {row.name.slice(0, 1).toUpperCase()}
            {/if}
          </span>
          <span class="name">{row.name}</span>
          <span class="count">{opening === row.id ? t("restore.waiting") : t("restore.windows", { count: row.count })}</span>
          <span class="arrow" aria-hidden="true">›</span>
        </button>
      {/each}
    {/if}
  </div>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
</main>

<style>
  main { display: flex; flex-direction: column; height: 100%; padding: 24px; background: var(--bg); color: var(--fg); font-family: var(--font); }
  header { display: flex; align-items: baseline; justify-content: space-between; gap: 12px; margin-bottom: 18px; }
  h1 { margin: 0; font-size: 21px; font-weight: 650; }
  header span, .count { color: var(--ctl-fg); font-size: 12px; white-space: nowrap; }
  .list { overflow: auto; min-height: 0; border-radius: 14px; }
  button { display: flex; width: 100%; align-items: center; gap: 12px; padding: 12px; border: 1px solid var(--line); border-radius: 12px; background: var(--card); color: inherit; font: inherit; text-align: left; cursor: pointer; }
  button + button { margin-top: 6px; }
  button:hover, button:focus-visible { border-color: var(--accent); outline: none; }
  button:disabled { cursor: wait; }
  .app-icon { display: grid; place-items: center; flex: 0 0 34px; height: 34px; border-radius: 9px; background: var(--pill); color: var(--accent); font-weight: 650; }
  img { width: 24px; height: 24px; object-fit: contain; }
  .name { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 14px; font-weight: 600; }
  .arrow { color: var(--accent); font-size: 20px; line-height: 1; }
  .empty { margin: 0; padding: 38px 12px; color: var(--ctl-fg); text-align: center; font-size: 14px; }
  .error { margin: 12px 0 0; color: var(--red); font-size: 13px; }
</style>
