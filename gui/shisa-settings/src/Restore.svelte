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
  let busyLock = $state(false);
  let agentMissing = $state(false);
  // True when the daemon reports at least one protected app. Null means the
  // list could not be read, so the lock action stays disabled rather than
  // offering a call that is known to be pointless.
  let hasProtected = $state(null);
  let notice = $state("");
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
    // The window can stay open while the tray, the CLI or a focus-loss
    // relock changes what is hidden. A focus reload keeps the list honest.
    const onFocus = () => {
      if (!opening && !busyLock) load(false);
    };
    window.addEventListener("focus", onFocus);
    return () => {
      unlisten.then((stop) => stop()).catch(() => {});
      window.removeEventListener("focus", onFocus);
    };
  });

  async function focusFirst() {
    await tick();
    list?.querySelector("button:not(:disabled)")?.focus();
  }

  async function load(focus = true) {
    error = "";
    agentMissing = false;
    try {
      const [counts, installed, protectedRows] = await Promise.all([
        invoke("hidden_apps"),
        invoke("list_installed").catch(() => []),
        invoke("list_apps").catch(() => null),
      ]);
      const apps = new Map(installed.map(([id, name, icon]) => [id, { name, icon }]));
      hasProtected = protectedRows ? protectedRows.length > 0 : null;
      // A valid ticket restores the window without a prompt. When the daemon
      // cannot answer, the hint stays unknown rather than guessed.
      const ticketed = protectedRows
        ? new Set(
            protectedRows
              .filter(([, unlocked]) => unlocked)
              .map(([id]) => id),
          )
        : null;
      rows = Object.entries(counts)
        .filter(([, count]) => count > 0)
        .map(([id, count]) => ({
          id,
          count,
          name: id,
          needsAuth: ticketed ? !ticketed.has(id) : null,
          ...apps.get(id),
        }))
        .sort((left, right) => left.name.localeCompare(right.name));
      for (const row of rows) {
        if (row.icon && !(row.icon in icons)) {
          invoke("icon", { name: row.icon }).then((icon) => {
            icons[row.icon] = icon;
          }).catch(() => {});
        }
      }
    } catch {
      // A missing agent is not a read error: it explains why the tray and
      // window hiding are unavailable and offers the command that fixes it.
      agentMissing = !(await invoke("agent_status").catch(() => false));
      hasProtected = null;
      rows = [];
      if (!agentMissing) error = t("restore.error.read");
    } finally {
      loading = false;
      if (focus) await focusFirst();
    }
  }

  /// The agent answers RestoreWindow before KWin has applied it. Poll the
  /// inventory so the panel reloads after the window is gone instead of
  /// leaving a stale row that still looks hidden.
  async function waitRestored(id) {
    for (let attempt = 0; attempt < 20; attempt++) {
      const counts = await invoke("hidden_apps").catch(() => null);
      if (!counts || !(counts[id] > 0)) return;
      await new Promise((resolve) => setTimeout(resolve, 150));
    }
  }

  async function restore(id) {
    if (opening || busyLock) return;
    opening = id;
    notice = "";
    error = "";
    try {
      await invoke("restore_hidden", { appId: id });
      await waitRestored(id);
      // Keep the row disabled until the refreshed list is painted, so a
      // second click cannot race the first restore.
      await load(false);
      opening = "";
      // Keep the panel open for the remaining apps. It closes only when the
      // list is empty, so restoring three apps is not three round trips.
      if (!agentMissing && !error && rows.length === 0) {
        await panel.close();
      } else {
        await focusFirst();
      }
    } catch (reason) {
      const message = typeof reason === "string" ? reason : t("restore.error.restore");
      // The window may have closed since the panel listed it. Reload so the
      // row does not stay clickable, then show the reason.
      await load(false);
      opening = "";
      error = message;
      await focusFirst();
    }
  }

  /// Lock revokes the daemon's state before the agent has applied it in
  /// KWin. Wait for a concealed window to appear so the refreshed list is
  /// not the one from before the lock. A lock with nothing to hide pays the
  /// short timeout instead of showing a stale list.
  async function waitLocked() {
    for (let attempt = 0; attempt < 20; attempt++) {
      const counts = await invoke("hidden_apps").catch(() => null);
      if (!counts) return;
      if (Object.values(counts).some((count) => count > 0)) return;
      await new Promise((resolve) => setTimeout(resolve, 150));
    }
  }

  async function lockAll() {
    if (busyLock || hasProtected !== true) return;
    busyLock = true;
    notice = "";
    error = "";
    try {
      await invoke("lock", { appId: "" });
      await waitLocked();
      await load(false);
      if (!error) notice = t("restore.lockedAll");
    } catch (reason) {
      const message = typeof reason === "string" ? reason : t("restore.error.lock");
      await load(false);
      error = message;
    } finally {
      busyLock = false;
    }
    await focusFirst();
  }

  async function openSettings() {
    try {
      await invoke("open_settings");
      await panel.close();
    } catch (reason) {
      error = typeof reason === "string" ? reason : t("restore.error.settings");
    }
  }

  async function retry() {
    loading = true;
    notice = "";
    await load();
  }

  /// Start a drag on a single left click. The usual
  /// `data-tauri-drag-region` treats a double click as maximize, which
  /// bypasses this window's fixed size.
  function startDrag(event) {
    if (event.button !== 0 || event.detail !== 1) return;
    if (event.target instanceof Element && event.target.closest("button")) return;
    panel.startDragging();
  }

  function keydown(event) {
    if (event.key === "Escape" && !opening && !busyLock) {
      panel.close();
    }
    if (opening || busyLock || (event.key !== "ArrowDown" && event.key !== "ArrowUp")) return;
    const buttons = [...(list?.querySelectorAll("button:not(:disabled)") ?? [])];
    if (!buttons.length) return;
    event.preventDefault();
    const index = buttons.indexOf(document.activeElement);
    const next = index < 0 ? 0 : (index + (event.key === "ArrowDown" ? 1 : -1) + buttons.length) % buttons.length;
    buttons[next].focus();
  }
</script>

<svelte:window onkeydown={keydown} />

<main>
  <div class="top" onmousedown={startDrag}>
    <div class="crumb"><b>{t("restore.title")}</b></div>
    <div class="actions">
      <button class="icon winclose" aria-label={t("top.close")} title={t("top.close")} onclick={() => panel.close()}>
        <svg viewBox="0 0 16 16" width="10" height="10"><path d="M4 4l8 8M12 4l-8 8" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/></svg>
      </button>
    </div>
  </div>
  <div class="scroll" bind:this={list} aria-busy={loading || !!opening || busyLock}>
    {#if loading}
      <div class="empty" role="status">{t("restore.loading")}</div>
    {:else if agentMissing}
      <div class="blank">
        <div class="blank-name">{t("restore.agentMissing")}</div>
        <div class="blank-sub">{t("restore.agentMissing.sub")}</div>
        <div class="blank-actions">
          <button class="text action" onclick={retry}>{t("restore.retry")}</button>
          <button class="text action" onclick={openSettings}>{t("restore.openSettings")}</button>
        </div>
      </div>
    {:else if !rows.length}
      <div class="blank">
        {#if !error}<div class="blank-name">{t("restore.empty")}</div>{/if}
        {#if notice}<div class="blank-sub">{notice}</div>{/if}
        <div class="blank-actions">
          <button
            class="text action"
            disabled={busyLock || hasProtected !== true}
            title={hasProtected === true ? undefined : hasProtected === false ? t("top.lockAllNone") : t("daemon.offline")}
            onclick={lockAll}
          >{t("restore.lockAll")}</button>
          <button class="text action" onclick={openSettings}>{t("restore.openSettings")}</button>
        </div>
      </div>
    {:else}
      <section class="set">
        <div class="label">{t("restore.windows", { count: total })}</div>
        <div class="list">
          {#each rows as row (row.id)}
            <button
              type="button"
              class="prefrow click row"
              disabled={!!opening || busyLock}
              onclick={() => restore(row.id)}
            >
              {#if icons[row.icon]}
                <img class="app-ic" src={icons[row.icon]} alt="" />
              {:else}
                <span class="app-ic"></span>
              {/if}
              <span class="who">
                <span class="name">{row.name}</span>
                <span class="sub">
                  {opening === row.id
                    ? t("restore.waiting")
                    : row.needsAuth === true
                      ? t("restore.auth", { count: row.count })
                      : row.needsAuth === false
                        ? t("restore.ticketed", { count: row.count })
                        : t("restore.windows", { count: row.count })}
                </span>
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
    padding: 0;
    background: var(--bg);
    color: var(--fg);
  }

  .scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 10px 16px 16px;
  }

  .scroll > .empty {
    text-align: center;
  }

  .blank {
    padding: 28px 8px 8px;
    text-align: center;
  }

  .blank-name {
    color: var(--fg-2);
    font-size: 13px;
  }

  .blank-sub {
    margin-top: 6px;
    color: var(--muted);
    font-size: 12px;
    line-height: 1.5;
    overflow-wrap: anywhere;
  }

  .blank-actions {
    display: flex;
    gap: 8px;
    justify-content: center;
    margin-top: 16px;
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
