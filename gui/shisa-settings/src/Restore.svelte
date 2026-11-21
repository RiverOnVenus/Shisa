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
  const total = $derived(rows.reduce((sum, row) => sum + row.count, 0));

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
  <div class="scroll" bind:this={list} aria-busy={loading || !!opening}>
    {#if loading}
      <div class="empty" role="status">{t("restore.loading")}</div>
    {:else if !rows.length}
      {#if !error}<div class="empty">{t("restore.empty")}</div>{/if}
    {:else}
      <section class="set">
        <div class="label">{t("restore.windows", { count: total })}</div>
        <div class="list">
          {#each rows as row (row.id)}
            <button
              type="button"
              class="prefrow click row"
              disabled={!!opening}
              onclick={() => restore(row.id)}
            >
              {#if icons[row.icon]}
                <img class="app-ic" src={icons[row.icon]} alt="" />
              {:else}
                <span class="app-ic"></span>
              {/if}
              <span class="who">
                <span class="name">{row.name}</span>
                <span class="sub">{opening === row.id ? t("restore.waiting") : t("restore.windows", { count: row.count })}</span>
              </span>
              <svg class="chev" viewBox="0 0 16 16" width="16" height="16" aria-hidden="true">
                <path d="m6 4 4 4-4 4" />
              </svg>
            </button>
          {/each}
        </div>
      </section>
    {/if}
  </div>
  {#if error}
    <div class="banner" role="alert">
      <span>{error}</span>
      <button class="text" onclick={() => (error = "")}>{t("common.close")}</button>
    </div>
  {/if}
</main>

<style>
  main {
    display: flex;
    flex-direction: column;
    height: 100%;
    padding: 10px 16px 16px;
    background: var(--bg);
    color: var(--fg);
  }

  .scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }

  .scroll > .empty {
    text-align: center;
  }

  /* the row is the settings window's .prefrow, kept as a button */
  .row {
    width: 100%;
  }

  .row .who {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
  }

  .row:disabled {
    cursor: wait;
  }

  .chev {
    flex: none;
    color: var(--muted);
    fill: none;
    stroke: currentColor;
    stroke-linecap: round;
    stroke-linejoin: round;
    stroke-width: 1.5;
  }
</style>
