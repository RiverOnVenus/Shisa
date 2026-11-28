<script>
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { flip } from "svelte/animate";
  import { cubicOut } from "svelte/easing";
  import { onMount, tick, untrack } from "svelte";
  import { fade, fly } from "svelte/transition";
  import { createTranslator, languageFromTag } from "./i18n.js";
  import { iconPixels } from "./format.js";
  import { motionMs, paintTheme } from "./motion.js";

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
  // A fast read should not flash a skeleton over the result.
  let loadingShown = $state(false);
  let closeTimer = 0;
  let opening = $state("");
  // The panel focuses its first control when it opens so Enter works at
  // once, but a pointer open should not paint the keyboard ring. The ring
  // waits for the first key press.
  let keyboard = $state(false);

  $effect(() => {
    if (loading) {
      const timer = setTimeout(() => (loadingShown = true), 120);
      return () => {
        clearTimeout(timer);
        loadingShown = false;
      };
    }
    loadingShown = false;
  });
  let busyLock = $state(false);
  let agentMissing = $state(false);
  // True when the daemon reports at least one protected app. Null means the
  // list could not be read, so the lock action stays disabled rather than
  // offering a call that is known to be pointless.
  let hasProtected = $state(null);
  let notice = $state("");
  let error = $state("");
  let list;
  let closeButton;
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

  let themed = false;
  function theme(dark) {
    if (dark === null) return;
    // The portal's first answer is still the opening paint, not a switch.
    paintTheme(dark, !themed);
    themed = true;
  }

  onMount(() => {
    // A signal that arrives before the first read resolves is newer than
    // that read. Let it paint, and discard the read so it cannot put the
    // previous scheme back.
    let live = false;
    const unlisten = listen("color_scheme", (event) => {
      if (typeof event.payload === "boolean") live = true;
      theme(event.payload);
    });
    invoke("color_scheme")
      .then((dark) => {
        if (!live) theme(dark);
      })
      .catch(() => {});
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
      clearTimeout(closeTimer);
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
      // A window hidden during the last-restore beat keeps the panel open.
      if (rows.length > 0) {
        clearTimeout(closeTimer);
        closeTimer = 0;
      }
      for (const row of rows) {
        if (row.icon && !(row.icon in icons)) {
          invoke("icon", {
            name: row.icon,
            px: iconPixels(window.devicePixelRatio),
          }).then((icon) => {
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
      // Keep the panel open for the remaining apps. When the last window is
      // back, show the empty state for a beat first: vanishing on the click
      // would leave the user without a visible result.
      if (!agentMissing && !error && rows.length === 0) {
        notice = t("restore.restored");
        clearTimeout(closeTimer);
        closeTimer = setTimeout(() => panel.close(), 560);
        // Enter was just used to restore. Moving focus to the empty state's
        // lock button would let a key repeat hide every protected window, so
        // the safe target is the close button.
        closeButton?.focus();
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
  /// KWin. The report carries this check's counts, but the panel's list
  /// comes from the inventory message that follows. Wait for a concealed
  /// window only when the report says something was hidden, so a lock with
  /// nothing to hide does not pay the short timeout.
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
      const report = await invoke("lock", { appId: "", language: lang });
      if (report.waitForInventory) await waitLocked();
      await load(false);
      if (!error) notice = report.summary;
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
    keyboard = true;
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

<main class:kbd={keyboard}>
  <div class="top" role="presentation" onmousedown={startDrag}>
    <div class="crumb"><b>{t("restore.title")}</b></div>
    <div class="actions">
      <button
        class="icon"
        aria-label={t("restore.settings")}
        title={t("restore.settings")}
        onclick={openSettings}
      >
        <svg viewBox="0 0 16 16" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M7.22 3.06L6.95 1.38A6.7 6.7 0 0 1 9.05 1.38L8.78 3.06A5 5 0 0 1 11.89 4.85L13.21 3.78A6.7 6.7 0 0 1 14.25 5.6L12.67 6.21A5 5 0 0 1 12.67 9.79L14.25 10.4A6.7 6.7 0 0 1 13.21 12.22L11.89 11.15A5 5 0 0 1 8.78 12.94L9.05 14.62A6.7 6.7 0 0 1 6.95 14.62L7.22 12.94A5 5 0 0 1 4.11 11.15L2.79 12.22A6.7 6.7 0 0 1 1.75 10.4L3.33 9.79A5 5 0 0 1 3.33 6.21L1.75 5.6A6.7 6.7 0 0 1 2.79 3.78L4.11 4.85A5 5 0 0 1 7.22 3.06Z" />
          <circle cx="8" cy="8" r="2.2" />
        </svg>
      </button>
      <button
        class="icon winclose"
        aria-label={t("top.close")}
        title={t("top.close")}
        bind:this={closeButton}
        onclick={() => panel.close()}
      >
        <svg viewBox="0 0 16 16" width="16" height="16"><path d="M3 3l10 10M13 3l-10 10" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/></svg>
      </button>
    </div>
  </div>
  <div class="scroll" bind:this={list} aria-busy={loading || !!opening || busyLock}>
    {#if loading}
      {#if loadingShown}
        <section class="set" role="status">
          <div class="label">{t("restore.loading")}</div>
          <div class="list">
            <div class="prefrow">
              <span class="sk sk-ic"></span>
              <div class="who"><span class="sk sk-name"></span><span class="sk sk-sub"></span></div>
            </div>
            <div class="prefrow">
              <span class="sk sk-ic"></span>
              <div class="who"><span class="sk sk-name"></span><span class="sk sk-sub"></span></div>
            </div>
          </div>
        </section>
      {/if}
    {:else if agentMissing}
      <div class="blank" in:fade={{ duration: motionMs(160) }}>
        <div class="blank-name">{t("restore.agentMissing")}</div>
        <div class="blank-sub">{t("restore.agentMissing.sub")}</div>
        <div class="blank-actions">
          <button class="text action" onclick={retry}>{t("restore.retry")}</button>
          <button class="text" onclick={openSettings}>{t("restore.openSettings")}</button>
        </div>
      </div>
    {:else if !rows.length}
      <div class="blank" in:fade={{ duration: motionMs(160) }}>
        {#if !error}
          <div class="blank-name">{t("restore.empty")}</div>
          <div class="blank-sub">
            {hasProtected === false ? t("restore.empty.none") : t("restore.empty.sub")}
          </div>
        {/if}
        {#if notice}<div class="blank-sub">{notice}</div>{/if}
        <div class="blank-actions">
          <button class="text action" onclick={openSettings}>{t("restore.openSettings")}</button>
          {#if hasProtected !== false}
            <button
              class="text"
              disabled={busyLock || hasProtected !== true}
              title={hasProtected === true ? t("restore.lockAllHint") : t("daemon.offline")}
              onclick={lockAll}
            >{t("restore.lockAll")}</button>
          {/if}
        </div>
      </div>
    {:else}
      <section class="set" in:fade={{ duration: motionMs(160) }}>
        <div class="label">{t("restore.windows", { count: total })}</div>
        <div class="list">
          {#each rows as row (row.id)}
            <button
              type="button"
              class="prefrow click row"
              in:fade={{ duration: motionMs(140) }}
              animate:flip={{ duration: motionMs(160), easing: cubicOut }}
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
                <span class="sub row-state">
                  {#if opening === row.id}<span class="row-spinner" aria-hidden="true"></span>{/if}
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
    <div
      class="banner"
      role="alert"
      transition:fly={{ y: 8, duration: motionMs(160), easing: cubicOut }}
    >
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

  /* The auto-focus on open is for Enter, not for showing a ring. */
  main:not(.kbd) button:focus-visible {
    outline: none;
  }

  .scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 10px 16px 16px;
    display: flex;
    flex-direction: column;
  }

  .scroll > .set {
    flex: none;
  }

  .blank {
    flex: 1;
    display: flex;
    flex-direction: column;
    justify-content: center;
    padding: 16px 8px;
    text-align: center;
  }

  .blank-name {
    color: var(--fg);
    font-size: var(--text-lg);
    font-weight: 600;
  }

  .blank-sub {
    margin-top: 8px;
    color: var(--muted);
    font-size: var(--text-sm);
    line-height: 1.55;
    overflow-wrap: anywhere;
  }

  .blank-actions {
    display: flex;
    gap: 8px;
    justify-content: center;
    margin-top: 20px;
  }

  /* The empty state's two actions are not equal. Opening settings is the step
     forward, locking is a guarded side action, so one is a bordered chip and
     the other stays plain. Filling one would add the only solid control in an
     app that otherwise keeps controls flat on the page. */
  .blank-actions .text {
    height: 30px;
    padding: 0 10px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }

  .blank-actions .action {
    padding: 0 12px;
    color: var(--fg);
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

  .row-state {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  .row-spinner {
    flex: none;
    width: 11px;
    height: 11px;
    border-radius: 50%;
    border: 1.5px solid var(--line);
    border-top-color: var(--fg-2);
    animation: row-spin 0.8s linear infinite;
  }

  @keyframes row-spin {
    to {
      transform: rotate(360deg);
    }
  }

  .row:disabled {
    cursor: wait;
  }

  /* the app id, shown only when two apps share a display name */
  .row .id {
    font-family: var(--code);
    font-size: var(--text-xs);
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
    transition: transform var(--dur-fast) var(--ease-out);
  }

  .row:not(:disabled):hover .chev {
    transform: translateX(2px);
  }
</style>
