// Pure rules of the settings window: how times read, how launch outcomes are
// named, and which apps a search shows. Kept out of App.svelte so `npm test`
// can cover them without a browser. Functions that produce text take the
// translator `t` from i18n.js.

// Grace periods the picker offers, paired with their text key.
const grace = [
  [0, "grace.every"],
  [300, "grace.5m"],
  [1800, "grace.30m"],
  [3600, "grace.1h"],
];

// Focus-loss delays the picker offers.
const relock = [
  [0, "relock.off"],
  [1, "relock.now"],
  [60, "relock.1m"],
  [300, "relock.5m"],
  [1800, "relock.30m"],
];

export const gracePresets = (t) => grace.map(([secs, key]) => [secs, t(key)]);
export const relockPresets = (t) => relock.map(([secs, key]) => [secs, t(key)]);

// How a launch ended: [outcome class, word key, reason key]. The audit list
// says the word and the reason, and the class drives the colour.
const kinds = {
  granted: ["ok", "outcome.allow", "outcome.granted"],
  ticket: ["ok", "outcome.allow", "outcome.ticket"],
  instance: ["ok", "outcome.allow", "outcome.instance"],
  autostart: ["ok", "outcome.allow", "outcome.autostart"],
  selfexec: ["ok", "outcome.allow", "outcome.selfexec"],
  asked: ["wait", "outcome.wait", "outcome.asked"],
  timeout: ["bad", "outcome.refuse", "outcome.timeout"],
  // The check returned after the wait limit had already passed.
  cancelled: ["bad", "outcome.refuse", "outcome.timeout"],
  failed: ["bad", "outcome.refuse", "outcome.failed"],
  // polkit's clean "not authorized": the user cancelled or gave up.
  denied: ["bad", "outcome.refuse", "outcome.denied"],
  // A lock took the app back before the answer could commit.
  revoked: ["bad", "outcome.refuse", "outcome.revoked"],
};

export const kindText = (kind, t) => {
  const entry = kinds[kind];
  return entry ? t(entry[1]) : kind;
};
export const reasonText = (kind, t) => {
  const entry = kinds[kind];
  return entry ? t(entry[2]) : "";
};
export const outcomeClass = (kind) => kinds[kind]?.[0] ?? "wait";

// The lock result the restore panel and settings window show. A missing
// result (an older agent, or a lock that did not ask for a check) is
// unconfirmed, never a success. Unknown statuses from a newer agent are
// treated the same way.
export function lockNotice(result, t, nameOf = (id) => id) {
  if (!result || result.status === "unconfirmed") {
    return t("lock.notice.unconfirmed");
  }
  if (result.status === "no-window-layer") {
    return t("lock.notice.noLayer");
  }
  if (result.status === "partial") {
    const missed = result.missed ?? [];
    if (missed.length === 0) return t("lock.notice.unconfirmed");
    const shown = missed.slice(0, 3).map((item) => nameOf(item.app));
    let apps = shown.join(t("lock.notice.separator"));
    if (missed.length > shown.length) {
      apps += t("lock.notice.more", { count: missed.length - shown.length });
    }
    return t("lock.notice.partial", {
      hidden: result.hidden ?? 0,
      matched: result.matched ?? 0,
      count: result.matched ?? 0,
      apps,
    });
  }
  if (result.status !== "confirmed") {
    return t("lock.notice.unconfirmed");
  }
  if (typeof result.matched !== "number") {
    return t("lock.notice.unconfirmed");
  }
  if (!result.matched) return t("lock.notice.none");
  const hidden = result.hidden ?? 0;
  if (hidden < result.matched) return t("lock.notice.unconfirmed");
  return t("lock.notice.confirmed", {
    hidden,
    matched: result.matched,
    count: result.matched,
  });
}

export function ago(at, now, t) {
  const s = Math.max(0, Math.round((now - at) / 1000));
  if (s < 5) return t("ago.justNow");
  if (s < 60) return t("ago.seconds", { count: s });
  const m = Math.round(s / 60);
  if (m < 60) return t("ago.minutes", { count: m });
  return t("ago.hours", { count: Math.round(m / 60) });
}

export const seconds = (ms) => Math.max(0, Math.round(ms / 1000));

// A span of time in the unit that reads best, for the diagnostics card.
export function duration(ms, t) {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return t("duration.seconds", { count: s });
  const m = Math.round(s / 60);
  if (m < 60) return t("duration.minutes", { count: m });
  const h = Math.round(m / 60);
  return h < 48
    ? t("duration.hours", { count: h })
    : t("duration.days", { count: Math.round(h / 24) });
}

export const isPreset = (value) => grace.some(([secs]) => secs === value);
export const isRelockPreset = (value) =>
  relock.some(([secs]) => secs === value);

// The muted second line of a log entry: the numbers behind the decision.
// For a box, the time from the launch being held to its answer.
export function detail(row, now, t) {
  const parts = [`pid ${row.pid}`];
  if (row.kind === "asked") {
    parts.push(t("detail.waiting", { count: seconds(now - row.at) }));
  } else if (
    row.resolved_at &&
    row.at &&
    ["granted", "denied", "timeout", "cancelled", "failed"].includes(row.kind)
  ) {
    parts.push(
      t("detail.authenticated", { count: seconds(row.resolved_at - row.at) }),
    );
  }
  return parts.join(" · ");
}

// Whether an app row belongs in the sidebar for this search and filter.
export function matches(app, query, filter) {
  const needle = query.trim().toLowerCase();
  if (
    needle &&
    !app.name.toLowerCase().includes(needle) &&
    !app.id.toLowerCase().includes(needle)
  )
    return false;
  if (filter === "protected" && !app.protected) return false;
  if (filter === "unlocked" && !(app.protected && app.unlocked)) return false;
  return true;
}
