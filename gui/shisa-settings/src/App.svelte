<script>
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { onMount, tick, untrack } from "svelte";
  import {
    ago,
    detail,
    duration,
    gracePresets,
    isPreset,
    isRelockPreset,
    kindText,
    matches,
    outcomeClass,
    reasonText,
    relockPresets,
  } from "./format.js";
  import {
    createTranslator,
    languageFromTag,
    languages,
    plural,
  } from "./i18n.js";

  let { language: initialLanguage = "en" } = $props();
  // The language is read once when the window mounts. Later changes come
  // from the selector below, so the prop is deliberately not reactive.
  let lang = $state(untrack(() => languageFromTag(initialLanguage)));
  const t = $derived(createTranslator(lang));

  const win = getCurrentWindow();
  const edges = [
    ["n", "North"], ["s", "South"], ["e", "East"], ["w", "West"],
    ["ne", "NorthEast"], ["nw", "NorthWest"], ["se", "SouthEast"], ["sw", "SouthWest"],
  ];
  const filters = ["all", "protected", "unlocked"];
  // The account that holds the app-lock password, as created by
  // data/shisa-lock.sysusers. The first run has no password yet,
  // and every authentication fails until root sets one.
  const adminUser = "shisa-lock";

  let apps = $state([]); // {id, name, icon, protected, unlocked, paths, effective}
  let query = $state("");
  let filter = $state("all");
  let currentId = $state(null);
  // "app" is the selected application's page, "general" the shared settings,
  // "activity" the live list of the daemon's launch decisions.
  let page = $state("app");

  // App page. `ready` keeps the controls hidden until the daemon answered, so
  // they never show defaults and then jump.
  let config = $state({ grace: 300, autostart: false, relock: 0, protected: false, ready: false });
  let resolved = $state([]);
  let notes = $state([]);
  // Whether the grace picker shows its number field. Set when the stored value
  // is not one of the presets, or when the custom button is clicked.
  let graceApp = $state(false);
  // Whether the relock picker shows its number field.
  let relockApp = $state(false);

  let global = $state({ grace: 300, clearOnLock: true, promptTimeout: 60 });
  let graceGlobal = $state(false);
  let newPath = $state("");
  // Bumped by every full refresh and every authenticated change, so a poll
  // that started earlier cannot put its pre-change snapshot back.
  let generation = 0;
  let globalReady = $state(false);

  let busy = $state(0);
  // What the busy overlay says. A lock takes no authentication, so it must
  // not claim to be waiting for one.
  let busyLabel = $state("");
  let error = $state("");
  let icons = $state({});
  // Whether shisad answered the last list request. The sidebar foot says it.
  let daemon = $state(false);
  // Whether shisa-agent owns its session bus name. The tray and window
  // hiding are separate from shisad, so they get their own line.
  let agent = $state(false);
  let appVersion = $state("");
  // The general page's search narrows the cards to those whose words it
  // holds. It reads the rendered cards, so it cannot drift from what the page
  // says.
  let settingsQuery = $state("");
  // The language card renders before the daemon answers, so the search reads
  // it separately from the cards below it.
  let languageCard = $state(null);
  // The element the general page's cards live in, for the search filter.
  let settingsRoot = $state(null);

  // The activity page: the daemon's recent launch decisions and the totals
  // it keeps since it started.
  let launches = $state([]);
  let activityCounts = $state({ requests: 0, allowed: 0, denied: 0 });
  let activityNote = $state("");
  // "all" or "denied". The audit list is worth reading mostly for refusals.
  let activityFilter = $state("all");
  // The general page's diagnostics card, read from the daemon. Null when the
  // daemon is too old to answer.
  let diag = $state(null);
  // After an upgrade the old shisad keeps running until its service is
  // restarted. The two versions can then disagree about D-Bus methods and
  // settings, so say so instead of letting the window fail method by method.
  const versionMismatch = $derived(
    Boolean(diag && appVersion && diag.version !== appVersion),
  );

  const nameOf = (id) => apps.find((a) => a.id === id)?.name ?? id;

  const current = $derived(apps.find((a) => a.id === currentId) ?? null);
  // Several desktop files can share one name; those rows show the id too.
  const nameCounts = $derived.by(() => {
    const counts = new Map();
    for (const app of apps) counts.set(app.name, (counts.get(app.name) ?? 0) + 1);
    return counts;
  });
  const visible = $derived(apps.filter((a) => matches(a, query, filter)));
  // The audit list, newest first. The refusals-only filter keeps the rows
  // that ended without the user's yes.
  const failure = $derived([...launches].reverse().find((row) => row.app === currentId && outcomeClass(row.kind) === "bad"));
  const activityRows = $derived(
    [...launches]
      .reverse()
      .filter((row) => activityFilter === "all" || outcomeClass(row.kind) === "bad"),
  );

  function filterSettings() {
    // The language card renders before the daemon answers, so it is searched
    // with the cards the daemon fills in. Comparisons ignore case so an
    // English query matches a capitalised label.
    const cards = [
      languageCard,
      ...(settingsRoot?.querySelectorAll(".set") ?? []),
    ].filter(Boolean);
    if (!cards.length) return;
    const needle = settingsQuery.trim().toLowerCase();
    let shown = 0;
    for (const card of cards) {
      const hit = !needle || card.textContent.toLowerCase().includes(needle);
      card.hidden = !hit;
      if (hit) shown++;
    }
    const none = document.querySelector(".set-none");
    if (none) none.hidden = shown > 0;
  }



  function fail(e) {
    error = e instanceof Error ? e.message : String(e);
  }

  /// Save a new interface language. This is the shell's own preference, so
  /// no authentication is involved. The window re-renders from `lang`, and
  /// the tray follows the same file on its own.
  async function chooseLanguage(tag) {
    if (tag === lang) return;
    try {
      await invoke("set_ui_language", { language: tag });
    } catch (e) {
      fail(e);
      return;
    }
    // Build the title from the new language directly, so it does not wait
    // for the derived translator to be recomputed.
    const title = createTranslator(tag)("app.title");
    lang = tag;
    document.documentElement.lang = tag;
    document.title = title;
    // The settings search reads the rendered cards, so re-apply it to the
    // new text once they have rendered again.
    await tick();
    filterSettings();
    // The title is cosmetic. A compositor that refuses it must not undo the
    // saved choice in the window.
    await win.setTitle(title).catch(() => {});
  }

  $effect(() => {
    document.title = t("app.title");
  });

  async function refresh() {
    generation++;
    const [installed, protectedList] = await Promise.all([
      invoke("list_installed"),
      invoke("list_apps"),
    ]);
    daemon = true;
    const rows = [];
    const byId = new Map();
    for (const [id, name, icon] of installed) {
      const row = { id, name: name || id, icon, protected: false, unlocked: false, paths: [], effective: [] };
      rows.push(row);
      byId.set(id, row);
    }
    for (const [id, unlocked, paths, effective] of protectedList) {
      let row = byId.get(id);
      if (!row) {
        row = { id, name: id, icon: "", protected: false, unlocked: false, paths: [], effective: [] };
        rows.push(row);
        byId.set(id, row);
      }
      row.protected = true;
      row.unlocked = unlocked;
      row.paths = paths;
      row.effective = effective;
    }
    apps = rows;
    loadIcons(rows);
  }

  async function loadIcons(rows) {
    // Each lookup runs on the shell's blocking pool. Asking for all of them
    // at once keeps a long app list from painting its icons one by one.
    await Promise.all(
      rows.map(async (row) => {
        if (!row.icon || row.id in icons) return;
        try {
          icons[row.id] = await invoke("icon", { name: row.icon });
        } catch {
          icons[row.id] = null;
        }
      }),
    );
  }

  async function select(id) {
    page = "app";
    currentId = id;
    void loadActivity();
    newPath = "";
    const row = apps.find((a) => a.id === id);
    config = { grace: 0, autostart: false, relock: 0, protected: row?.protected === true, ready: false };
    resolved = [];
    notes = [];
    try {
      const cfg = await invoke("app_config", { appId: id });
      // The user may have moved on while the daemon was answering.
      if (currentId !== id || page !== "app") return;
      config = { grace: cfg[0], autostart: cfg[1], relock: cfg[3], protected: cfg[2], ready: true };
      graceApp = !isPreset(cfg[0]);
      relockApp = !isRelockPreset(cfg[3]);
    } catch (e) {
      if (currentId === id && page === "app") fail(e);
      return;
    }
    // A protected app whose desktop file is gone still shows its switch and
    // paths, so it can be unprotected. Only the coverage card reports the
    // failed lookup.
    try {
      const res = await invoke("resolve_app", { appId: id });
      if (currentId !== id || page !== "app") return;
      resolved = res[0];
      notes = res[1];
    } catch (e) {
      if (currentId === id && page === "app") {
        notes = [e instanceof Error ? e.message : String(e)];
      }
    }
  }

  async function selectGeneral() {
    page = "general";
    currentId = null;
    globalReady = false;
    settingsQuery = "";
    await Promise.all([loadGlobal(), loadDiag()]);
  }

  /// The live page: the daemon's recent launch decisions, polled while it is
  /// open. The animation is driven by ids that were not on the page before.
  async function selectActivity() {
    page = "activity";
    currentId = null;
    await loadActivity();
  }

  async function loadActivity() {
    if (page !== "activity" && page !== "app") return;
    let rows, counts;
    try {
      [rows, ...counts] = await invoke("recent_launches");
    } catch {
      activityNote = daemon
        ? t("activity.note.old")
        : t("activity.note.offline");
      return;
    }
    // A poll that raced the page switch puts nothing back.
    if (page !== "activity" && page !== "app") return;
    activityNote = "";
    [activityCounts.requests, activityCounts.allowed, activityCounts.denied] = counts;
    // `done` came with the daemon that records when a box was answered; an
    // older one stops at `until`, and the page still renders without it.
    const next = rows.map(([id, at, app, pid, kind, until, done]) => ({
      id,
      at,
      app,
      pid,
      kind,
      until,
      resolved_at: done ?? 0,
    }));
    launches = next;
  }

  async function loadGlobal() {
    try {
      const [grace, clearOnLock, promptTimeout] = await invoke("global_config");
      global = { grace, clearOnLock, promptTimeout };
      graceGlobal = !isPreset(grace);
      globalReady = true;
      // The cards render after this assignment. Wait for them so the first
      // search, possibly typed while the daemon was answering, reaches them.
      await tick();
      filterSettings();
    } catch (e) {
      if (page === "general") fail(e);
    }
  }

  /// The daemon's own numbers. An older daemon has no such method, and then
  /// the card says so instead of the window failing.
  async function loadDiag() {
    try {
      const [
        version,
        started,
        reloaded,
        failed,
        apps,
        marked,
        config,
        adminReady,
        configFailed,
      ] = await invoke("diagnostics");
      diag = {
        version,
        started,
        reloaded,
        failed,
        apps,
        marked,
        config,
        adminReady,
        configFailed,
      };
    } catch {
      diag = null;
    }
  }

  /// Run a change that needs authentication and then reload what it touched.
  /// Both halves report their own error, and `after` runs once either way so
  /// a failed reload cannot leave an unhandled rejection behind.
  async function change(action, after, label = null) {
    // Keyboard activation can reach a focused control through the busy
    // overlay, which only blocks the pointer.
    if (busy > 0) return;
    busyLabel = label ?? t("busy.auth");
    busy++;
    generation++;
    error = "";
    try {
      await action();
    } catch (e) {
      fail(e);
    }
    try {
      await after();
    } catch (e) {
      fail(e);
    } finally {
      busy--;
    }
  }

  function setProtected(on) {
    const id = currentId;
    change(
      () => invoke("set_protected", { appId: id, on }),
      async () => {
        await refresh();
        if (currentId === id) await select(id);
      },
    );
  }

  function setGrace(seconds) {
    // A click on the value already in effect is not a change, and must not
    // open an authentication box.
    if (seconds === config.grace) return;
    const id = currentId;
    change(
      () => invoke("set_grace", { appId: id, seconds }),
      async () => {
        if (currentId === id) await select(id);
      },
    );
  }

  function setAutostart(allow) {
    if (allow === config.autostart) return;
    const id = currentId;
    change(
      () => invoke("set_autostart", { appId: id, allow }),
      async () => {
        if (currentId === id) await select(id);
      },
    );
  }

  function setRelock(seconds) {
    if (seconds === config.relock) return;
    const id = currentId;
    change(
      () => invoke("set_relock", { appId: id, seconds }),
      async () => {
        if (currentId === id) await select(id);
      },
    );
  }

  function addPath() {
    const id = currentId;
    const path = newPath.trim();
    if (!path || (current?.paths ?? []).includes(path)) return;
    if (!path.startsWith("/")) {
      error = t("app.addPath.absolute");
      return;
    }
    const paths = [...(current?.paths ?? []), path];
    newPath = "";
    change(
      () => invoke("set_paths", { appId: id, paths }),
      async () => {
        await refresh();
        if (currentId === id) await select(id);
      },
    );
  }

  function removePath(path) {
    const id = currentId;
    const paths = (current?.paths ?? []).filter((p) => p !== path);
    change(
      () => invoke("set_paths", { appId: id, paths }),
      async () => {
        await refresh();
        if (currentId === id) await select(id);
      },
    );
  }

  function lockAll() {
    change(() => invoke("lock", { appId: "" }), refresh, t("busy.lock"));
  }

  /// Drop this app's ticket without authentication: locking only ever takes
  /// something away.
  function lockNow() {
    const id = currentId;
    change(() => invoke("lock", { appId: id }), syncTickets, t("busy.lock"));
  }

  /// Ask for a ticket now, the way `shisa unlock` does. The box answers first,
  /// then the row's unlocked mark changes.
  function unlockNow() {
    const id = currentId;
    change(
      () => invoke("unlock", { appId: id }),
      syncTickets,
    );
  }

  /// Open an app's own page from its audit row, when the app still exists.
  function openApp(id) {
    if (apps.some((app) => app.id === id)) select(id);
  }

  function setGlobalGrace(seconds) {
    if (seconds === global.grace) return;
    change(
      () => invoke("set_grace", { appId: "", seconds }),
      loadGlobal,
    );
  }

  function setClearOnLock(on) {
    change(
      () => invoke("set_clear_on_lock", { on }),
      loadGlobal,
    );
  }

  function setPromptTimeout(seconds) {
    if (seconds === global.promptTimeout) return;
    change(
      () => invoke("set_prompt_timeout", { seconds }),
      loadGlobal,
    );
  }

  /// An emptied or malformed number field restores the value it showed
  /// instead of submitting 0, which would mean "authenticate every launch".
  function setNumber(input, previous, apply) {
    const text = input.value.trim();
    const value = Number(text);
    if (text === "" || !Number.isFinite(value)) {
      input.value = String(previous);
      return;
    }
    apply(value);
  }

  /// The list's protection and unlocked marks go stale as tickets expire and
  /// other tools change protection. Refresh only those fields in place, so the
  /// rows, the open page and the selection stay where they are.
  async function syncTickets() {
    const gen = generation;
    try {
      const list = await invoke("list_apps");
      // A full refresh or a change finished while this poll was in flight.
      if (gen !== generation) return;
      daemon = true;
      const protectedIds = new Set();
      let missing = false;
      for (const [id, unlocked, paths, effective] of list) {
        protectedIds.add(id);
        const row = apps.find((a) => a.id === id);
        if (row) {
          row.protected = true;
          row.unlocked = unlocked;
          row.paths = paths;
          row.effective = effective;
        } else {
          // Protected from the CLI or another window while this window was
          // open: the row does not exist yet.
          missing = true;
        }
      }
      for (const row of apps) {
        if (!protectedIds.has(row.id)) {
          row.protected = false;
          row.unlocked = false;
          row.paths = [];
          row.effective = [];
        }
      }
      if (missing && gen === generation) await refresh();
      // Someone else changed protection or this page is simply older than
      // the daemon. Reload it rather than leave a switch that does the
      // opposite of what it shows.
      const row = page === "app" && currentId ? apps.find((a) => a.id === currentId) : null;
      if (row && config.ready && row.protected !== config.protected) {
        await select(currentId);
      }
    } catch {
      daemon = false;
      // Keep the last good snapshot. A user action or the next tick retries.
    }
  }

  async function checkAgent() {
    agent = await invoke("agent_status").catch(() => false);
  }

  async function refreshTickets() {
    if (busy > 0) return;
    if (apps.length === 0) {
      // The window may have opened before shisad was running. A full refresh
      // on this tick brings the list back once it is.
      try {
        await refresh();
        if (!currentId && page === "app" && apps.length) await select(apps[0].id);
      } catch {
        // Still down; the next tick tries again.
      }
      return;
    }
    await syncTickets();
  }

  onMount(() => {
    invoke("app_version")
      .then((version) => (appVersion = version))
      .catch(() => {});
    void checkAgent();
    (async () => {
      try {
        await refresh();
        await loadDiag();
        if (apps.length) await select(apps[0].id);
      } catch (e) {
        daemon = false;
        fail(e);
      }
    })();

    // The desktop's appearance comes from the XDG portal (main.rs), because
    // WebKitGTK follows GTK, which KDE keeps light under a dark Plasma. A
    // null answer means no portal or no preference, and then the media query
    // keeps deciding. main.js has already painted from that query.
    const media = matchMedia("(prefers-color-scheme: dark)");
    let followed = false;
    const paint = (dark) => {
      followed = typeof dark === "boolean";
      const darkNow = followed ? dark : media.matches;
      document.documentElement.dataset.theme = darkNow ? "dark" : "light";
    };
    const onMedia = () => {
      if (!followed) paint(null);
    };
    const unlisten = listen("color_scheme", (event) => paint(event.payload)).catch(
      () => () => {},
    );
    invoke("color_scheme")
      .then(paint)
      .catch(() => paint(null));
    media.addEventListener("change", onMedia);

    const tick = setInterval(() => { refreshTickets(); checkAgent(); if (page === "app") loadActivity(); }, 10000);
    // The activity page follows the daemon while it is open. 800ms keeps a
    // waiting box's answer within a blink of the list.
    const live = setInterval(() => { if (page === "activity") loadActivity(); }, 800);
    const onFocus = () => { refreshTickets(); checkAgent(); };
    window.addEventListener("focus", onFocus);
    return () => {
      clearInterval(tick);
      clearInterval(live);
      window.removeEventListener("focus", onFocus);
      media.removeEventListener("change", onMedia);
      unlisten.then((off) => off());
    };
  });
</script>

{#snippet secondsPicker(value, custom, setCustom, apply, presets, max, step)}
  <div class="val grace">
    <div class="segs">
      {#each presets as [secs, label]}
        <button
          class="opt"
          class:on={!custom && value === secs}
          onclick={() => {
            setCustom(false);
            apply(secs);
          }}>{label}</button>
      {/each}
      <button class="opt" class:on={custom} onclick={() => setCustom(true)}>{t("general.custom")}</button>
    </div>
    {#if custom}
      <input
        class="num"
        type="number"
        min="0"
        max={max}
        step={step}
        value={value}
        onchange={(e) => setNumber(e.currentTarget, value, apply)}
      />
      <span class="unit">{t("common.seconds")}</span>
    {/if}
  </div>
{/snippet}

{#each edges as [id, direction]}
  <button
    class="edge edge-{id}"
    tabindex="-1"
    aria-hidden="true"
    onpointerdown={() => win.startResizeDragging(direction)}
  ></button>
{/each}

<div class="app">
  <aside class="sidebar">
    <div class="side-top" data-tauri-drag-region>
      <span class="brand" data-tauri-drag-region>
        <svg class="logo" viewBox="0.75 0.75 14.5 14.5" width="16" height="16" fill="currentColor" fill-rule="evenodd" aria-hidden="true">
          <path d="M8 6.6a4.2 4.2 0 1 1 0 8.4 4.2 4.2 0 1 1 0-8.4Z M8 8a2.8 2.8 0 1 0 0 5.6 2.8 2.8 0 1 0 0-5.6Z"/>
          <path d="M5 1h6l4 3v3.25L12.3 9.6h-2.05L9 11H7L5.75 9.6H3.7L1 7.25V4Z M3 5l3.4 1v1.2H4.75L3 5.8Z M13 5l-3.4 1v1.2h1.65L13 5.8Z"/>
        </svg>
        <span>Shisa</span>
      </span>
    </div>

    <div class="side-scroll">
      <input
        class="search"
        type="search"
        spellcheck="false"
        autocomplete="off"
        aria-label={t("sidebar.search")}
        placeholder={t("sidebar.search")}
        bind:value={query}
      />

      <div class="segs filters">
        {#each filters as key}
          <button class="opt" class:on={filter === key} onclick={() => (filter = key)}>{t(`filters.${key}`)}</button>
        {/each}
      </div>

      <div class="nav-gl">{t("sidebar.group")}</div>
      <ul class="list">
        {#each visible as app (app.id)}
          <li>
            <button class="row" class:on={page === "app" && app.id === currentId} onclick={() => select(app.id)}>
              {#if icons[app.id]}
                <img src={icons[app.id]} alt="" />
              {:else}
                <span class="icon-gap"></span>
              {/if}
              <span class="who">
                <span class="name">{app.name}</span>
                {#if (nameCounts.get(app.name) ?? 0) > 1}<span class="sub">{app.id}</span>{/if}
              </span>
              {#if app.protected}
                <span class="dot" class:open={app.unlocked} title={app.unlocked ? t("dot.unlocked") : t("dot.locked")}></span>
              {/if}
            </button>
          </li>
        {/each}
        {#if visible.length === 0}
          <li class="empty-state">{query || filter !== "all" ? t("sidebar.empty.match") : t("sidebar.empty.none")}</li>
        {/if}
      </ul>
    </div>

    <div class="side-foot">
      <button class="row" class:on={page === "general"} onclick={selectGeneral}>
        <!-- Drawn here rather than looked up as `preferences-system`: the
             theme lookup only scans apps/ and can miss, and the file's own
             colours would not follow the row's. -->
        <svg viewBox="0 0 16 16" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M7.22 3.06L6.95 1.38A6.7 6.7 0 0 1 9.05 1.38L8.78 3.06A5 5 0 0 1 11.89 4.85L13.21 3.78A6.7 6.7 0 0 1 14.25 5.6L12.67 6.21A5 5 0 0 1 12.67 9.79L14.25 10.4A6.7 6.7 0 0 1 13.21 12.22L11.89 11.15A5 5 0 0 1 8.78 12.94L9.05 14.62A6.7 6.7 0 0 1 6.95 14.62L7.22 12.94A5 5 0 0 1 4.11 11.15L2.79 12.22A6.7 6.7 0 0 1 1.75 10.4L3.33 9.79A5 5 0 0 1 3.33 6.21L1.75 5.6A6.7 6.7 0 0 1 2.79 3.78L4.11 4.85A5 5 0 0 1 7.22 3.06Z" />
          <circle cx="8" cy="8" r="2.2" />
        </svg>
        <span class="who"><span class="name">{t("sidebar.general")}</span></span>
      </button>
      <div class="gw" class:off={!daemon} title={daemon ? t("daemon.online") : t("daemon.offline")}><i></i><span>{daemon ? t("daemon.online") : t("daemon.offline")}</span></div>
      <div class="gw" class:off={!agent} title={agent ? t("agent.hint.on") : t("agent.hint.off")}><i></i><span>{agent ? t("agent.online") : t("agent.offline")}</span></div>
    </div>
  </aside>

  <div class="main">
    <div class="top" data-tauri-drag-region>
      <div class="crumb" data-tauri-drag-region>
        <span>{page === "general" ? t("crumb.settings") : page === "activity" ? t("crumb.diagnostics") : t("crumb.apps")}</span>
        <i>/</i>
        <b>{page === "general" ? t("crumb.general") : page === "activity" ? t("crumb.activity") : current?.name ?? "—"}</b>
      </div>
      {#if page === "general"}
        <input
          class="search top-q"
          type="search"
          spellcheck="false"
          autocomplete="off"
          aria-label={t("top.search")}
          placeholder={t("top.search")}
          bind:value={settingsQuery}
          oninput={filterSettings}
        />
      {/if}
      <div class="actions">
        {#if page === "activity" || (page === "app" && current)}
          <button class="icon" aria-label={t("top.lockAll")} title={t("top.lockAll")} onclick={lockAll}>
            <svg viewBox="0 0 16 16" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <rect x="3.5" y="7" width="9" height="6.5" rx="1.2" />
              <path d="M5.5 7V5.2a2.5 2.5 0 0 1 5 0V7" />
            </svg>
          </button>
        {/if}
        <button class="icon" aria-label={t("top.minimize")} title={t("top.minimize")} onclick={() => win.minimize()}>
          <svg viewBox="0 0 16 16" width="14" height="14"><path d="M3.5 8h9" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/></svg>
        </button>
        <button class="icon winclose" aria-label={t("top.close")} title={t("top.close")} onclick={() => win.close()}>
          <svg viewBox="0 0 16 16" width="10" height="10"><path d="M4 4l8 8M12 4l-8 8" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/></svg>
        </button>
      </div>
    </div>

    <div class="view">
      <div class="col">
        {#if diag && diag.adminReady === false}
          <section class="set notice">
            <div class="list">
              <div class="prefrow">
                <div class="who">
                  <div class="name">{t("password.name")}</div>
                  <div class="sub">
                    {t("password.sub")}
                    <code>sudo passwd {adminUser}</code>{t("password.after")}
                  </div>
                </div>
              </div>
            </div>
          </section>
        {/if}
        {#if versionMismatch}
          <section class="set notice">
            <div class="list">
              <div class="prefrow">
                <div class="who">
                  <div class="name">{t("version.name")}</div>
                  <div class="sub">
                    {t("version.sub", { daemon: diag.version, app: appVersion })}
                  </div>
                </div>
              </div>
            </div>
          </section>
        {/if}
      {#if page === "general"}
        <!-- The language is a local preference, so it stays usable while the
             daemon is down and the rest of this page cannot load. -->
        <section class="set" bind:this={languageCard}>
          <div class="list">
            <div class="prefrow">
              <div class="who">
                <div class="name">{t("general.language.name")}</div>
              </div>
              <div class="segs two">
                {#each languages as [tag, name]}
                  <button class="opt" class:on={lang === tag} onclick={() => chooseLanguage(tag)}>{name}</button>
                {/each}
              </div>
            </div>
          </div>
        </section>
        {#if globalReady}
          <div bind:this={settingsRoot}>
          <section class="set">
            <div class="list">
              <div class="prefrow">
                <div class="who">
                  <div class="name">{t("general.grace.name")}</div>
                  <div class="sub">{t("general.grace.sub")}</div>
                </div>
                {@render secondsPicker(global.grace, graceGlobal, (on) => (graceGlobal = on), setGlobalGrace, gracePresets(t), 31536000, 30)}
              </div>
              <div class="prefrow">
                <div class="who">
                  <div class="name">{t("general.clear.name")}</div>
                  <div class="sub">{t("general.clear.sub")}</div>
                </div>
                <button
                  class="lib-switch"
                  class:on={global.clearOnLock}
                  role="switch"
                  aria-checked={global.clearOnLock}
                  aria-label={t("general.clear.name")}
                  onclick={() => setClearOnLock(!global.clearOnLock)}
                ><i></i></button>
              </div>
              <div class="prefrow">
                <div class="who">
                  <div class="name">{t("general.timeout.name")}</div>
                  <div class="sub">{t("general.timeout.sub")}</div>
                </div>
                <div class="val">
                  <input
                    class="num"
                    type="number"
                    min="5"
                    max="85"
                    value={global.promptTimeout}
                    onchange={(e) => setNumber(e.currentTarget, global.promptTimeout, setPromptTimeout)}
                  />
                  <span class="unit">{t("general.timeout.unit")}</span>
                </div>
              </div>
            </div>
          </section>
          <section class="set">
            <div class="label">{t("diag.records")}</div>
            <div class="list"><div class="prefrow">
              <button class="text action" onclick={selectActivity}>{t("diag.view")}</button>
            </div></div>
          </section>
          {#if diag}
            <section class="set">
              <div class="label">{t("diag.title")}</div>
              <div class="list">
                <div class="prefrow">
                  <div class="who">
                    <div class="name">shisad {diag.version}</div>
                    <div class="sub">
                      {t("diag.uptime", {
                        duration: duration(Date.now() - diag.started, t),
                        reloaded: diag.reloaded
                          ? t("diag.reloaded.at", { ago: ago(diag.reloaded, Date.now(), t) })
                          : t("diag.reloaded.never"),
                      })}
                    </div>
                  </div>
                </div>
                <div class="prefrow">
                  <div class="who">
                    <div class="name">{t("diag.apps.name")}</div>
                    <div class="sub">{t("diag.apps.sub", {
                      apps: diag.apps,
                      marked: diag.marked,
                      appWord: plural(lang, diag.apps, "app", "apps"),
                      pathWord: plural(lang, diag.marked, "path", "paths"),
                    })}</div>
                  </div>
                </div>
                <div class="prefrow">
                  <div class="who">
                    <div class="name">{t("diag.last.name")}</div>
                    <div class="sub" class:bad={diag.failed > 0}>
                      {diag.failed
                        ? t("diag.last.failed", { ago: ago(diag.failed, Date.now(), t) })
                        : t("diag.last.ok")}
                    </div>
                  </div>
                </div>
                {#if diag.configFailed > 0}
                  <div class="prefrow">
                    <div class="who">
                      <div class="name">{t("diag.configFailed.name")}</div>
                      <div class="sub bad">
                        {t("diag.configFailed.sub", {
                          ago: ago(diag.configFailed, Date.now(), t),
                        })}
                      </div>
                    </div>
                  </div>
                {/if}
                {#if diag.config}
                  <div class="prefrow">
                    <div class="who">
                      <div class="name">{t("diag.config")}</div>
                      <div class="sub mono">{diag.config}</div>
                    </div>
                  </div>
                {/if}
              </div>
            </section>
          {:else}
            <section class="set">
              <div class="list">
                <div class="prefrow">
                  <div class="who">
                    <div class="sub">{t("diag.unavailable")}</div>
                  </div>
                </div>
              </div>
            </section>
          {/if}
          </div>
          <div class="empty set-none" hidden>{t("settings.empty.match")}</div>
        {:else}
          <div class="empty">{t("settings.loading")}</div>
        {/if}
      {:else if page === "activity"}
        <div class="act-head">
          <div class="label">{t("activity.title")}</div>
          <div class="segs">
            <button class="opt" class:on={activityFilter === "all"} onclick={() => (activityFilter = "all")}>{t("activity.all")}</button>
            <button class="opt" class:on={activityFilter === "denied"} onclick={() => (activityFilter = "denied")}>{t("activity.denied")}</button>
          </div>
        </div>
        {#if activityNote}
          <div class="empty">{activityNote}</div>
        {:else}
          <p class="act-summary">
            {t("activity.summary", {
              requests: activityCounts.requests,
              allowed: activityCounts.allowed,
              denied: activityCounts.denied,
              launchWord: plural(lang, activityCounts.requests, "launch", "launches"),
            })}
          </p>
          <section class="set">
          <div class="list">
            {#each activityRows as row (row.id)}
              {@const openable = apps.some((app) => app.id === row.app)}
              <button
                type="button"
                class="prefrow"
                class:click={openable}
                disabled={!openable}
                title={openable ? t("activity.open") : undefined}
                onclick={() => openApp(row.app)}
              >
                {#if icons[row.app]}
                  <img class="app-ic" src={icons[row.app]} alt="" />
                {:else}
                  <span class="icon-gap"></span>
                {/if}
                <div class="who">
                  <div class="name">{nameOf(row.app)}<span class="when">{ago(row.at, Date.now(), t)}</span></div>
                  <div class="sub">{detail(row, Date.now(), t)}</div>
                </div>
                <span class="outcome {outcomeClass(row.kind)}">{kindText(row.kind, t)}<span class="why">{reasonText(row.kind, t)}</span></span>
              </button>
            {:else}
              <div class="prefrow">
                <div class="who">
                  {#if launches.length === 0}
                    <div class="sub">{t("activity.empty.none")}</div>
                  {:else}
                    <div class="sub">{t("activity.empty.denied")}</div>
                  {/if}
                </div>
              </div>
            {/each}
          </div>
          </section>
        {/if}
      {:else if current}
        <section class="set">
          <div class="list">
            <div class="prefrow">
              {#if icons[current.id]}<img class="app-ic" src={icons[current.id]} alt="" />{/if}
              <div class="who">
                <div class="name">{current.name}</div>
                <div class="sub">{current.id}</div>
              </div>
              {#if config.ready && current.protected}
                <div class="val">
                  <span class="state" class:open={current.unlocked}>{current.unlocked ? t("app.state.unlocked") : t("app.state.locked")}</span>
                  <button class="text action" onclick={lockNow}>{t("app.lockNow")}</button>
                  <button class="text action" onclick={unlockNow}>{t("app.unlockNow")}</button>
                </div>
              {/if}
            </div>
          </div>
        </section>

        {#if failure && !activityNote}
          <section class="set">
            <div class="list">
              <div class="prefrow"><div class="who"><div class="name">{t("app.failure")}</div>
                <div class="sub">{ago(failure.at, Date.now(), t)} · {reasonText(failure.kind, t)}</div>
              </div></div>
            </div>
          </section>
        {/if}
        {#if config.ready}
          <section class="set">
            <div class="list">
              <div class="prefrow">
                <div class="who">
                  <div class="name">{t("app.protect.name")}</div>
                  {#if config.protected}<div class="sub">{t("app.protect.sub.on")}</div>{/if}
                </div>
                <button
                  class="lib-switch"
                  class:on={config.protected}
                  role="switch"
                  aria-checked={config.protected}
                  aria-label={t("app.protect.name")}
                  onclick={() => setProtected(!config.protected)}
                ><i></i></button>
              </div>
              {#if config.protected}
                <div class="prefrow">
                  <div class="who">
                    <div class="name">{t("app.grace.name")}</div>
                    <div class="sub">{config.grace === 0 ? t("app.grace.sub.zero") : t("app.grace.sub")}</div>
                  </div>
                  {@render secondsPicker(config.grace, graceApp, (on) => (graceApp = on), setGrace, gracePresets(t), 31536000, 30)}
                </div>
                <div class="prefrow">
                  <div class="who">
                    <div class="name">{t("app.relock.name")}</div>
                    <div class="sub">{t("app.relock.sub")}</div>
                  </div>
                  <div class="segs">
                    {#each relockPresets(t) as [secs, label]}
                      <button class="opt" class:on={config.relock === secs} onclick={() => setRelock(secs)}>{label}</button>
                    {/each}
                    {#if relockApp}<span class="unit">{config.relock} {t("common.seconds")}</span>{/if}
                  </div>
                </div>
                <details class="advanced">
                <summary class="prefrow adv-toggle">
                  <span class="name">{t("app.advanced")}</span>
                  <svg class="chev" viewBox="0 0 16 16" aria-hidden="true">
                    <path d="m4 6 4 4 4-4" />
                  </svg>
                </summary>
                <div class="prefrow"><label>{t("app.customRelock")}
                  <input class="num" type="number" min="0" max="86400" value={config.relock}
                    onchange={(e) => setNumber(e.currentTarget, config.relock, setRelock)} /> {t("common.seconds")}
                </label></div>
                <div class="prefrow">
                  <div class="who">
                    <div class="name">{t("app.autostart.name")}</div>
                    <div class="sub">{t("app.autostart.sub")}</div>
                  </div>
                  <div class="segs two">
                    <button class="opt" class:on={!config.autostart} onclick={() => setAutostart(false)}>{t("app.autostart.prompt")}</button>
                    <button class="opt" class:on={config.autostart} onclick={() => setAutostart(true)}>{t("app.autostart.allow")}</button>
                  </div>
                </div>
                {#each current.paths ?? [] as path}
                  <div class="prefrow">
                    <div class="who">
                      <div class="name mono">{path}</div>
                      {#if !(current.effective ?? []).includes(path)}
                        <div class="sub bad">{t("app.path.inactive")}</div>
                      {/if}
                    </div>
                    <button
                      class="text danger"
                      disabled={(current.paths ?? []).length <= 1}
                      onclick={() => removePath(path)}
                    >{t("common.remove")}</button>
                  </div>
                {/each}
                <div class="prefrow">
                  <div class="who">
                    {#if (current.paths ?? []).length === 1}
                      <div class="sub">{t("app.path.keep")}</div>
                    {:else if (current.paths ?? []).length === 0}
                      <div class="sub">{t("app.path.none")}</div>
                    {/if}
                    <div class="add">
                      <input class="words" placeholder={t("app.path.placeholder")} bind:value={newPath} />
                      <button class="text action" onclick={addPath} disabled={!newPath.trim()}>{t("common.add")}</button>
                    </div>
                  </div>
                </div>
                </details>
              {/if}
            </div>
          </section>
        {:else}
          <div class="empty">{t("settings.loading")}</div>
        {/if}

          <section class="set">
            <div class="list">
              <div class="prefrow">
                <div class="who">
                  <div class="name">{t("app.coverage.name")}</div>
                  <div class="sub">{t("app.coverage.sub")}</div>
                </div>
              </div>
              <div class="prefrow">
                <div class="who">
                  <div class="name">{t("app.windows.name")}</div>
                  <div class="sub">{t("app.windows.sub")}</div>
                </div>
              </div>
              {#each (current.paths ?? []).filter((path) => !(current.effective ?? []).includes(path)) as path}
                <div class="prefrow"><div class="sub bad">{t("app.path.inactivePrefix")}{path}</div></div>
              {/each}
              {#each resolved as path}
                <div class="prefrow"><div class="who"><div class="name mono">{path}</div></div></div>
              {/each}
              {#if resolved.length === 0}
                <div class="prefrow"><div class="who"><div class="sub">{t("app.coverage.empty")}</div></div></div>
              {/if}
              {#each notes as note}
                <div class="prefrow"><div class="who"><div class="sub bad">{note}</div></div></div>
              {/each}
            </div>
          </section>
      {:else}
        <div class="empty">{t("app.none")}</div>
      {/if}
      </div>
    </div>
  </div>
</div>

{#if busy > 0}
  <div class="overlay" role="status" aria-live="polite">
    <div class="busy">
      <div class="spinner"></div>
      <span>{busyLabel}</span>
    </div>
  </div>
{/if}

{#if error}
  <div class="banner" role="alert">
    <span>{error}</span>
    <button class="text" onclick={() => (error = "")}>{t("common.close")}</button>
  </div>
{/if}
