<script>
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { onMount } from "svelte";

  const win = getCurrentWindow();
  const edges = [
    ["n", "North"], ["s", "South"], ["e", "East"], ["w", "West"],
    ["ne", "NorthEast"], ["nw", "NorthWest"], ["se", "SouthEast"], ["sw", "SouthWest"],
  ];
  const filters = [["all", "全部"], ["protected", "受保护"], ["unlocked", "已解锁"], ["unprotected", "未受保护"]];
  // Grace periods the picker offers. Anything else shows as 自定义.
  const gracePresets = [[0, "每次"], [300, "5 分钟"], [1800, "30 分钟"], [3600, "1 小时"]];
  // How long a window may sit unfocused before it is hidden again. 1 hides
  // it as soon as the focus leaves.
  const relockPresets = [[0, "关闭"], [1, "立即"], [60, "1 分钟"], [300, "5 分钟"], [1800, "30 分钟"]];
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
  // is not one of the presets, or when 自定义 is clicked.
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
  let gear = $state(null);
  // Whether shisad answered the last list request. The sidebar foot says it.
  let daemon = $state(false);
  // The general page's search narrows the cards to those whose words it
  // holds. It reads the rendered cards, so it cannot drift from what the page
  // says.
  let settingsQuery = $state("");
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

  const nameOf = (id) => apps.find((a) => a.id === id)?.name ?? id;

  // How a launch ended: [in a word, why]. The audit list says both.
  const kinds = {
    granted: ["放行", "认证通过"],
    ticket: ["放行", "票据"],
    instance: ["放行", "进程"],
    autostart: ["放行", "自启动"],
    selfexec: ["放行", "子进程"],
    asked: ["等待", "认证中"],
    timeout: ["拒绝", "超时"],
    // The check returned after the wait limit had already passed.
    cancelled: ["拒绝", "超时"],
    failed: ["拒绝", "出错"],
    // polkit's clean "not authorized": the user cancelled or gave up.
    denied: ["拒绝", "未通过"],
  };
  const kindText = (kind) => kinds[kind]?.[0] ?? kind;
  const reasonText = (kind) => kinds[kind]?.[1] ?? "";
  const outcomeClass = (kind) => (kinds[kind]?.[0] === "放行" ? "ok" : kinds[kind]?.[0] === "拒绝" ? "bad" : "wait");

  function ago(at) {
    const s = Math.max(0, Math.round((Date.now() - at) / 1000));
    if (s < 5) return "刚刚";
    if (s < 60) return `${s} 秒前`;
    const m = Math.round(s / 60);
    if (m < 60) return `${m} 分钟前`;
    return `${Math.round(m / 60)} 小时前`;
  }

  const seconds = (ms) => Math.max(0, Math.round(ms / 1000));

  /// A span of time in the unit that reads best, for the diagnostics card.
  function duration(ms) {
    const s = Math.max(0, Math.round(ms / 1000));
    if (s < 60) return `${s} 秒`;
    const m = Math.round(s / 60);
    if (m < 60) return `${m} 分钟`;
    const h = Math.round(m / 60);
    return h < 48 ? `${h} 小时` : `${Math.round(h / 24)} 天`;
  }

  const isPreset = (value) => gracePresets.some(([secs]) => secs === value);
  const isRelockPreset = (value) => relockPresets.some(([secs]) => secs === value);

  /// The muted second line of a log entry: the numbers behind the decision.
  /// For a box, the time from the launch being held to its answer.
  function detail(row) {
    const parts = [`pid ${row.pid}`];
    if (row.kind === "asked") {
      parts.push(`已等 ${seconds(Date.now() - row.at)} 秒`);
    } else if (row.resolved_at && row.at && ["granted", "denied", "timeout", "cancelled", "failed"].includes(row.kind)) {
      parts.push(`认证用时 ${seconds(row.resolved_at - row.at)} 秒`);
    }
    return parts.join(" · ");
  }

  const current = $derived(apps.find((a) => a.id === currentId) ?? null);
  // Several desktop files can share one name; those rows show the id too.
  const nameCounts = $derived.by(() => {
    const counts = new Map();
    for (const app of apps) counts.set(app.name, (counts.get(app.name) ?? 0) + 1);
    return counts;
  });
  const visible = $derived(apps.filter((a) => matches(a)));
  // The audit list, newest first. 只看拒绝 keeps the rows that ended without
  // the user's yes.
  const failure = $derived([...launches].reverse().find((row) => row.app === currentId && outcomeClass(row.kind) === "bad"));
  const activityRows = $derived(
    [...launches]
      .reverse()
      .filter((row) => activityFilter === "all" || outcomeClass(row.kind) === "bad"),
  );

  function filterSettings() {
    if (!settingsRoot) return;
    const needle = settingsQuery.trim();
    let shown = 0;
    for (const card of settingsRoot.querySelectorAll(".set")) {
      const hit = !needle || card.textContent.includes(needle);
      card.hidden = !hit;
      if (hit) shown++;
    }
    const none = document.querySelector(".set-none");
    if (none) none.hidden = shown > 0;
  }



  function matches(a) {
    const needle = query.trim().toLowerCase();
    if (needle && !a.name.toLowerCase().includes(needle) && !a.id.toLowerCase().includes(needle))
      return false;
    if (filter === "protected" && !a.protected) return false;
    if (filter === "unlocked" && !(a.protected && a.unlocked)) return false;
    if (filter === "unprotected" && a.protected) return false;
    return true;
  }

  // The daemon answers in English. The window is Chinese, so the messages a
  // settings change or a desktop lookup can produce are translated here.
  // Anything else is shown as it came.
  const knownErrors = [
    ["too many pending authentication requests", "还有一个认证没有结束，完成或取消后再试。"],
    ["authentication was rejected", "认证没有通过，改动没有生效。"],
    ["authentication timed out", "认证超时，改动没有生效。"],
    ["authentication cancelled", "认证取消了，改动没有生效。"],
    ["not a regular file", "这个路径不是普通文件，要指向可执行文件。"],
    ["cannot read", "读不到这个路径，检查文件是否存在。"],
    ["not an absolute path", "保护路径要写绝对路径。"],
    ["is not protected", "这个应用已经不在保护列表里，刷新后再试。"],
    ["has no usable protected path", "这个应用没有可用的保护路径，检查 desktop 文件或手动添加路径。"],
    ["desktop file not found", "找不到这个应用的 desktop 文件。"],
    ["invalid app id", "应用 id 不合法。"],
    ["no home directory", "读不到这个账号的家目录。"],
    ["unknown user", "读不到这个账号的资料。"],
    ["serviceunknown", "应用锁没有响应，稍后再试。"],
    ["was not provided", "应用锁没有响应，稍后再试。"],
    ["connection refused", "应用锁没有响应，稍后再试。"],
  ];

  function readable(text) {
    const hay = text.toLowerCase();
    const hit = knownErrors.find(([needle]) => hay.includes(needle));
    return hit ? hit[1] : text;
  }

  function fail(e) {
    error = readable(e instanceof Error ? e.message : String(e));
  }

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
    for (const row of rows) {
      if (!row.icon || row.id in icons) continue;
      try {
        icons[row.id] = await invoke("icon", { name: row.icon });
      } catch {
        icons[row.id] = null;
      }
    }
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
        notes = [readable(e instanceof Error ? e.message : String(e))];
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
        ? "当前守护进程不提供活动记录，装上新版 shisad 后可用。"
        : "应用锁没有响应，等它恢复后这一页会自动更新。";
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
      filterSettings();
    } catch (e) {
      if (page === "general") fail(e);
    }
  }

  /// The daemon's own numbers. An older daemon has no such method, and then
  /// the card says so instead of the window failing.
  async function loadDiag() {
    try {
      const [version, started, reloaded, failed, apps, marked, config, adminReady] =
        await invoke("diagnostics");
      diag = { version, started, reloaded, failed, apps, marked, config, adminReady };
    } catch {
      diag = null;
    }
  }

  /// Run a change that needs authentication and then reload what it touched.
  /// Both halves report their own error, and `after` runs once either way so
  /// a failed reload cannot leave an unhandled rejection behind.
  async function change(action, after, label = "等待认证…") {
    // Keyboard activation can reach a focused control through the busy
    // overlay, which only blocks the pointer.
    if (busy > 0) return;
    busyLabel = label;
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
      error = "保护路径要写绝对路径，例如 /usr/bin/程序。";
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
    change(
      () => invoke("lock", { appId: "" }),
      refresh,
      "正在上锁…",
    );
  }

  /// Drop this app's ticket without authentication: locking only ever takes
  /// something away.
  function lockNow() {
    const id = currentId;
    change(
      () => invoke("lock", { appId: id }),
      syncTickets,
      "正在上锁…",
    );
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
    (async () => {
      try {
        gear = await invoke("icon", { name: "preferences-system" });
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

    const tick = setInterval(() => { refreshTickets(); if (page === "app") loadActivity(); }, 10000);
    // The activity page follows the daemon while it is open. 800ms keeps a
    // waiting box's answer within a blink of the list.
    const live = setInterval(() => { if (page === "activity") loadActivity(); }, 800);
    const onFocus = () => refreshTickets();
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
      <button class="opt" class:on={custom} onclick={() => setCustom(true)}>自定义</button>
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
      <span class="unit">秒</span>
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
        aria-label="搜索应用"
        placeholder="搜索应用"
        bind:value={query}
      />

      <div class="segs">
        {#each filters as [key, label]}
          <button class="opt" class:on={filter === key} onclick={() => (filter = key)}>{label}</button>
        {/each}
      </div>

      <div class="nav-gl">应用</div>
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
                <span class="dot" class:open={app.unlocked} title={app.unlocked ? "启动凭证有效" : "启动需认证"}></span>
              {/if}
            </button>
          </li>
        {/each}
        {#if visible.length === 0}
          <li class="empty-state">{query || filter !== "all" ? "没有匹配的应用" : "没有可显示的应用"}</li>
        {/if}
      </ul>
    </div>

    <div class="side-foot">
      <button class="row" class:on={page === "general"} onclick={selectGeneral}>
        {#if gear}<img src={gear} alt="" />{:else}<span class="icon-gap"></span>{/if}
        <span class="who"><span class="name">通用</span></span>
      </button>
      <div class="gw" class:off={!daemon}><i></i><span>{daemon ? "应用锁在运行" : "应用锁没有响应"}</span></div>
    </div>
  </aside>

  <div class="main">
    <div class="top" data-tauri-drag-region>
      <div class="crumb" data-tauri-drag-region>
        <span>{page === "general" ? "设置" : page === "activity" ? "通用 · 诊断" : "应用"}</span>
        <i>/</i>
        <b>{page === "general" ? "通用" : page === "activity" ? "最近启动" : current?.name ?? "—"}</b>
      </div>
      {#if page === "general"}
        <input
          class="search top-q"
          type="search"
          spellcheck="false"
          autocomplete="off"
          aria-label="查找设置"
          placeholder="查找设置"
          bind:value={settingsQuery}
          oninput={filterSettings}
        />
      {/if}
      <div class="actions">
        {#if page === "app" && current}
          <button class="text" onclick={lockAll}>全部上锁</button>
        {/if}
        <button class="icon" aria-label="最小化" onclick={() => win.minimize()}>
          <svg viewBox="0 0 16 16" width="14" height="14"><path d="M3.5 8h9" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/></svg>
        </button>
        <button class="icon winclose" aria-label="关闭" onclick={() => win.close()}>
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
                  <div class="name">还没设置应用锁密码</div>
                  <div class="sub">
                    受保护的应用要输入应用锁密码，这个密码还没设，现在所有认证都会失败。请管理员在终端运行
                    <code>sudo passwd {adminUser}</code>，完成后重新打开这个窗口。
                  </div>
                </div>
              </div>
            </div>
          </section>
        {/if}
      {#if page === "general"}
        {#if globalReady}
          <div bind:this={settingsRoot}>
          <section class="set">
            <div class="list">
              <div class="prefrow">
                <div class="who">
                  <div class="name">默认宽限期</div>
                  <div class="sub">认证通过后这段时间内再打开不再询问。单个应用可以在自己的页面里改。</div>
                </div>
                {@render secondsPicker(global.grace, graceGlobal, (on) => (graceGlobal = on), setGlobalGrace, gracePresets, 31536000, 30)}
              </div>
              <div class="prefrow">
                <div class="who">
                  <div class="name">锁屏时清除已通过的认证</div>
                  <div class="sub">开启时锁屏会收回已打开的受保护窗口，解锁后恢复要重新认证。关掉后锁屏不改变正在使用的窗口，只影响下一次启动。</div>
                </div>
                <button
                  class="lib-switch"
                  class:on={global.clearOnLock}
                  role="switch"
                  aria-checked={global.clearOnLock}
                  aria-label="锁屏时清除已通过的认证"
                  onclick={() => setClearOnLock(!global.clearOnLock)}
                ><i></i></button>
              </div>
              <div class="prefrow">
                <div class="who">
                  <div class="name">认证框等待上限</div>
                  <div class="sub">最多等这么久，超时就拒绝启动。取消或超时也会被系统记一次认证失败，连续 3 次后应用锁暂停约 10 分钟，不影响登录。</div>
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
                  <span class="unit">秒 · 5 到 85</span>
                </div>
              </div>
            </div>
          </section>
          <section class="set">
            <div class="label">诊断记录</div>
            <div class="list"><div class="prefrow">
              <button class="text action" onclick={selectActivity}>查看最近启动和拒绝原因</button>
            </div></div>
          </section>
          {#if diag}
            <section class="set">
              <div class="label">诊断</div>
              <div class="list">
                <div class="prefrow">
                  <div class="who">
                    <div class="name">shisad {diag.version}</div>
                    <div class="sub">
                      已运行 {duration(Date.now() - diag.started)}，最近一次重新加载{diag.reloaded
                        ? `在 ${ago(diag.reloaded)}`
                        : "还没发生"}。
                    </div>
                  </div>
                </div>
                <div class="prefrow">
                  <div class="who">
                    <div class="name">受保护的应用</div>
                    <div class="sub">{diag.apps} 个应用，{diag.marked} 条路径已加上监视标记。</div>
                  </div>
                </div>
                <div class="prefrow">
                  <div class="who">
                    <div class="name">最近一次加载</div>
                    <div class="sub" class:bad={diag.failed > 0}>
                      {diag.failed
                        ? `有路径没标记上，${ago(diag.failed)}`
                        : "全部路径已标记。"}
                    </div>
                  </div>
                </div>
                {#if diag.config}
                  <div class="prefrow">
                    <div class="who">
                      <div class="name">配置文件</div>
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
                    <div class="sub">当前守护进程不提供诊断信息，装上新版 shisad 后可用。</div>
                  </div>
                </div>
              </div>
            </section>
          {/if}
          </div>
          <div class="empty set-none" hidden>没有匹配的设置</div>
        {:else}
          <div class="empty">正在读取设置…</div>
        {/if}
      {:else if page === "activity"}
        <div class="act-head">
          <div class="label">最近启动</div>
          <div class="segs">
            <button class="opt" class:on={activityFilter === "all"} onclick={() => (activityFilter = "all")}>全部</button>
            <button class="opt" class:on={activityFilter === "denied"} onclick={() => (activityFilter = "denied")}>只看拒绝</button>
          </div>
          <span class="grow"></span>
          <button class="text" onclick={lockAll}>全部上锁</button>
        </div>
        {#if activityNote}
          <div class="empty">{activityNote}</div>
        {:else}
          <p class="act-summary">
            本次运行 {activityCounts.requests} 次启动，{activityCounts.allowed} 次放行，{activityCounts.denied} 次拒绝。记录只保存在 shisad 的内存里，重启后清空。
          </p>
          <section class="set">
          <div class="list">
            {#each activityRows as row (row.id)}
              <div
                class="prefrow click"
                role="button"
                tabindex="0"
                title={apps.some((a) => a.id === row.app) ? "打开这个应用的页面" : ""}
                onclick={() => openApp(row.app)}
                onkeydown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    openApp(row.app);
                  }
                }}
              >
                {#if icons[row.app]}
                  <img class="app-ic" src={icons[row.app]} alt="" />
                {:else}
                  <span class="icon-gap"></span>
                {/if}
                <div class="who">
                  <div class="name">{nameOf(row.app)}<span class="when">{ago(row.at)}</span></div>
                  <div class="sub">{detail(row)}</div>
                </div>
                <span class="outcome {outcomeClass(row.kind)}">{kindText(row.kind)}<span class="why">{reasonText(row.kind)}</span></span>
              </div>
            {:else}
              <div class="prefrow">
                <div class="who">
                  {#if launches.length === 0}
                    <div class="sub">还没有启动记录。打开一个受保护的应用，它的裁决会出现在这里。</div>
                  {:else}
                    <div class="sub">没有拒绝的记录。</div>
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
                  <span class="state" class:open={current.unlocked}>{current.unlocked ? "启动凭证有效" : "启动需认证"}</span>
                  <button class="text action" onclick={lockNow}>立即上锁</button>
                  <button class="text action" onclick={unlockNow}>立即解锁</button>
                </div>
              {/if}
            </div>
          </div>
        </section>

        {#if failure && !activityNote}
          <div class="prefrow"><div class="who"><div class="name">最近一次启动失败</div>
            <div class="sub">{ago(failure.at)} · {reasonText(failure.kind)}</div>
          </div></div>
        {/if}
        {#if config.ready}
          <section class="set">
            <div class="list">
              <div class="prefrow">
                <div class="who">
                  <div class="name">启动前要求认证</div>
                  <div class="sub">{config.protected ? "打开这个应用时用应用锁密码验证一次。取消或超时就拒绝启动。" : "开启后打开这个应用要先验证应用锁密码，下面是它的宽限期、自启动和保护路径。"}</div>
                </div>
                <button
                  class="lib-switch"
                  class:on={config.protected}
                  role="switch"
                  aria-checked={config.protected}
                  aria-label="启动前要求认证"
                  onclick={() => setProtected(!config.protected)}
                ><i></i></button>
              </div>
              {#if config.protected}
                <div class="prefrow">
                  <div class="who">
                    <div class="name">宽限期</div>
                    <div class="sub">{config.grace === 0 ? "0 表示每次启动都要认证。" : "认证通过后这段时间内再打开不再询问。锁屏是否清除看通用页的开关。"}</div>
                  </div>
                  {@render secondsPicker(config.grace, graceApp, (on) => (graceApp = on), setGrace, gracePresets, 31536000, 30)}
                </div>
                <div class="prefrow">
                  <div class="who">
                    <div class="name">失焦后隐藏</div>
                    <div class="sub">窗口失去焦点超过这段时间后自动隐藏，恢复要重新认证。到点之前切回来不会隐藏。立即表示一切换走就隐藏，关闭时窗口保持可见。</div>
                  </div>
                  <div class="segs">
                    {#each relockPresets as [secs, label]}
                      <button class="opt" class:on={config.relock === secs} onclick={() => setRelock(secs)}>{label}</button>
                    {/each}
                    {#if relockApp}<span class="unit">{config.relock} 秒</span>{/if}
                  </div>
                </div>
                <details><summary class="prefrow">高级设置</summary>
                <div class="prefrow"><label>自定义失焦时长
                  <input class="num" type="number" min="0" max="86400" value={config.relock}
                    onchange={(e) => setNumber(e.currentTarget, config.relock, setRelock)} /> 秒
                </label></div>
                <div class="prefrow">
                  <div class="who">
                    <div class="name">自启动</div>
                    <div class="sub">登录后 30 秒内从自启动单元启动时直接放行。这个单元名同账号可以伪造，只适合你信任自己登录后第一分钟的情况。</div>
                  </div>
                  <div class="segs two">
                    <button class="opt" class:on={!config.autostart} onclick={() => setAutostart(false)}>要求认证</button>
                    <button class="opt" class:on={config.autostart} onclick={() => setAutostart(true)}>放行</button>
                  </div>
                </div>
                {#each current.paths ?? [] as path}
                  <div class="prefrow">
                    <div class="who">
                      <div class="name mono">{path}</div>
                      {#if !(current.effective ?? []).includes(path)}
                        <div class="sub bad">未生效，当前没有被监视。检查文件是否存在，或者它是不是 shell、解释器。</div>
                      {/if}
                    </div>
                    <button
                      class="text danger"
                      disabled={(current.paths ?? []).length <= 1}
                      onclick={() => removePath(path)}
                    >移除</button>
                  </div>
                {/each}
                <div class="prefrow">
                  <div class="who">
                    {#if (current.paths ?? []).length === 1}
                      <div class="sub">至少保留一条保护路径。要放开这个应用，请关掉上面的保护开关。</div>
                    {:else if (current.paths ?? []).length === 0}
                      <div class="sub">还没有路径，下面加一个。</div>
                    {/if}
                    <div class="add">
                      <input class="words" placeholder="/usr/bin/程序 或 /opt/游戏/启动器" bind:value={newPath} />
                      <button class="text action" onclick={addPath} disabled={!newPath.trim()}>添加</button>
                    </div>
                  </div>
                </div>
                </details>
              {/if}
            </div>
          </section>
        {:else}
          <div class="empty">正在读取设置…</div>
        {/if}

          <section class="set">
            <div class="list">
              <div class="prefrow">
                <div class="who">
                  <div class="name">覆盖情况</div>
                  <div class="sub">下面列出从 desktop 文件解析出的入口。实际保护路径可在高级设置中查看。</div>
                </div>
              </div>
              <div class="prefrow">
                <div class="who">
                  <div class="name">已打开的窗口</div>
                  <div class="sub">锁屏后隐藏，再次打开需要认证。窗口层以用户身份运行，同账号的程序可以绕过。</div>
                </div>
              </div>
              {#each (current.paths ?? []).filter((path) => !(current.effective ?? []).includes(path)) as path}
                <div class="prefrow"><div class="sub bad">未生效：{path}</div></div>
              {/each}
              {#each resolved as path}
                <div class="prefrow"><div class="who"><div class="name mono">{path}</div></div></div>
              {/each}
              {#if resolved.length === 0}
                <div class="prefrow"><div class="who"><div class="sub">没有从 desktop 文件解析到可保护的入口。</div></div></div>
              {/if}
              {#each notes as note}
                <div class="prefrow"><div class="who"><div class="sub bad">{note}</div></div></div>
              {/each}
            </div>
          </section>
      {:else}
        <div class="empty">选择左侧的应用</div>
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
    <button class="text" onclick={() => (error = "")}>关闭</button>
  </div>
{/if}
