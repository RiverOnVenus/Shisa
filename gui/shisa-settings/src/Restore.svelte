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
  // A quiet reload is already reading. A second one would only paint the same
  // snapshot twice.
  let refreshing = false;
  // Bumped by every read. A read that started earlier cannot put its
  // pre-change snapshot back after a newer one finished.
  let generation = 0;
  // The panel keeps the desktop-entry index for a while. The daemon walks the
  // application directories for every ListInstalled call, so a two-second
  // poll must not repeat that work. A failed read waits less before retrying.
  let installed = null;
  let installedNextTry = 0;
  const INSTALLED_AGE = 30_000;
  const INSTALLED_RETRY = 5_000;
  const total = $derived(rows.reduce((sum, row) => sum + row.count, 0));
  // Several desktop files can share one display name. Those rows show the id
  // too, because the name alone would not tell them apart.
  const nameCounts = $derived.by(() => {
    const counts = new Map();
    for (const row of rows) counts.set(row.name, (counts.get(row.name) ?? 0) + 1);
    return counts;
  });

  function theme(dark) {
    if (dark !== null) document.documentElement.dataset.theme = dark ? "dark" : "light";
  }

  onMount(() => {
    const unlisten = listen("color_scheme", (event) => theme(event.payload));
    invoke("color_scheme").then(theme).catch(() => {});
    load();
    // The list can change while the panel stays open: a focus-loss hide can
    // fire just after the panel took focus, and the tray or the CLI can
    // restore or conceal an app. Poll quietly so the rows and their ticket
    // hints stay honest. A focus reload covers a window that was hidden
    // behind a dialog, and the minutes between polls.
    const poll = setInterval(() => {
      if (!document.hidden) void reload();
    }, 2000);
    const onFocus = () => void reload();
    window.addEventListener("focus", onFocus);
    return () => {
      unlisten.then((stop) => stop()).catch(() => {});
      clearInterval(poll);
      window.removeEventListener("focus", onFocus);
    };
  });

  /// Reload behind the user's back. It must not move focus, clear the notice
  /// from the last action or replace a shown error with a transient read
  /// failure.
  async function reload() {
    if (loading || refreshing || opening || busyLock) return;
    refreshing = true;
    try {
      await load(false, true);
    } finally {
      refreshing = false;
    }
  }

  async function focusFirst() {
    await tick();
    list?.querySelector("button:not(:disabled)")?.focus();
  }

  /// Names and icons for the hidden apps. A failed read keeps the last good
  /// index, and a missing entry falls back to the app id.
  async function installedApps() {
    if (Date.now() < installedNextTry) return installed;
    try {
      const rows = await invoke("list_installed");
      installed = new Map(rows.map(([id, name, icon]) => [id, { name, icon }]));
      installedNextTry = Date.now() + INSTALLED_AGE;
    } catch {
      // Keep the previous index, or none on the first read, and do not
      // repeat the daemon's directory walk on every poll.
      installedNextTry = Date.now() + INSTALLED_RETRY;
    }
    return installed;
  }

  /// `background` marks a refresh the user did not ask for. It keeps the
  /// notice from the last action, clears a read error once a read succeeds,
  /// and leaves the last good rows and names in place when the daemon cannot
  /// answer but the agent still does.
  async function load(focus = true, background = false) {
    const gen = ++generation;
    if (!background) error = "";
    try {
      // A failed daemon read is `null`, not an empty list: the rows then keep
      // the names and ticket hints they already showed.
      const [counts, apps, protectedRows] = await Promise.all([
        invoke("hidden_apps"),
        installedApps(),
        invoke("list_apps").catch(() => null),
      ]);
      // A newer read or a user action started while this one was in flight.
      // Its own reload knows the newer state, so this one is dropped.
      if (gen !== generation || (background && (opening || busyLock))) return;
      agentMissing = false;
      if (background && error === t("restore.error.read")) error = "";
      const previous = new Map(rows.map((row) => [row.id, row]));
      if (protectedRows) hasProtected = protectedRows.length > 0;
      else if (!background) hasProtected = null;
      // A valid ticket restores the window without a prompt. When the daemon
      // cannot answer, the hint stays as it was rather than guessed.
      const ticketed = protectedRows
        ? new Set(
            protectedRows
              .filter(([, unlocked]) => unlocked)
              .map(([id]) => id),
          )
        : null;
      rows = Object.entries(counts)
        .filter(([, count]) => count > 0)
        .map(([id, count]) => {
          const app = apps?.get(id);
          const old = previous.get(id);
          return {
            id,
            count,
            name: app?.name ?? old?.name ?? id,
            icon: app?.icon ?? old?.icon,
            needsAuth: ticketed ? !ticketed.has(id) : (old?.needsAuth ?? null),
          };
        })
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
      const missing = !(await invoke("agent_status").catch(() => false));
      if (gen !== generation) return;
      if (!missing && background) return;
      agentMissing = missing;
      hasProtected = null;
      rows = [];
      if (agentMissing) {
        // The missing-agent screen replaces the read-error banner, which
        // would otherwise stay on screen below it.
        if (error === t("restore.error.read")) error = "";
      } else {
        error = t("restore.error.read");
      }
    } finally {
      // The newest read owns the spinner and the focus.
      if (gen === generation) {
        loading = false;
        if (focus) await focusFirst();
      }
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
                {#if (nameCounts.get(row.name) ?? 0) > 1 && row.id !== row.name}
                  <span class="sub id">{row.id}</span>
                {/if}
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

  /* the app id, shown only when two apps share a display name */
  .row .id {
    font-family: var(--code);
    font-size: 11px;
    color: var(--muted);
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
