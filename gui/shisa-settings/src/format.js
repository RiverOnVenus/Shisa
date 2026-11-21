// Pure rules of the settings window: how times read, how launch outcomes are
// named, and which apps a search shows. Kept out of App.svelte so `npm test`
// can cover them without a browser.

// Grace periods the picker offers. Anything else shows as 自定义.
export const gracePresets = [
  [0, "每次"],
  [300, "5 分钟"],
  [1800, "30 分钟"],
  [3600, "1 小时"],
];

// How long a window may sit unfocused before it is hidden again. 1 hides it
// as soon as the focus leaves.
export const relockPresets = [
  [0, "关闭"],
  [1, "立即"],
  [60, "1 分钟"],
  [300, "5 分钟"],
  [1800, "30 分钟"],
];

// How a launch ended: [in a word, why]. The audit list says both.
export const kinds = {
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

export const kindText = (kind) => kinds[kind]?.[0] ?? kind;
export const reasonText = (kind) => kinds[kind]?.[1] ?? "";
export const outcomeClass = (kind) =>
  kinds[kind]?.[0] === "放行"
    ? "ok"
    : kinds[kind]?.[0] === "拒绝"
      ? "bad"
      : "wait";

export function ago(at, now = Date.now()) {
  const s = Math.max(0, Math.round((now - at) / 1000));
  if (s < 5) return "刚刚";
  if (s < 60) return `${s} 秒前`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m} 分钟前`;
  return `${Math.round(m / 60)} 小时前`;
}

export const seconds = (ms) => Math.max(0, Math.round(ms / 1000));

/// A span of time in the unit that reads best, for the diagnostics card.
export function duration(ms) {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s} 秒`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m} 分钟`;
  const h = Math.round(m / 60);
  return h < 48 ? `${h} 小时` : `${Math.round(h / 24)} 天`;
}

export const isPreset = (value) =>
  gracePresets.some(([secs]) => secs === value);
export const isRelockPreset = (value) =>
  relockPresets.some(([secs]) => secs === value);

/// The muted second line of a log entry: the numbers behind the decision.
/// For a box, the time from the launch being held to its answer.
export function detail(row, now = Date.now()) {
  const parts = [`pid ${row.pid}`];
  if (row.kind === "asked") {
    parts.push(`已等 ${seconds(now - row.at)} 秒`);
  } else if (
    row.resolved_at &&
    row.at &&
    ["granted", "denied", "timeout", "cancelled", "failed"].includes(row.kind)
  ) {
    parts.push(`认证用时 ${seconds(row.resolved_at - row.at)} 秒`);
  }
  return parts.join(" · ");
}

/// Whether an app row belongs in the sidebar for this search and filter.
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
  if (filter === "unprotected" && app.protected) return false;
  return true;
}

// The daemon answers in English. The window is Chinese, so the messages a
// settings change or a desktop lookup can produce are translated here.
// Anything else is shown as it came.
export const knownErrors = [
  [
    "too many pending authentication requests",
    "还有一个认证没有结束，完成或取消后再试。",
  ],
  ["authentication was rejected", "认证没有通过，改动没有生效。"],
  ["authentication timed out", "认证超时，改动没有生效。"],
  ["authentication cancelled", "认证取消了，改动没有生效。"],
  ["not a regular file", "这个路径不是普通文件，要指向可执行文件。"],
  ["cannot read", "读不到这个路径，检查文件是否存在。"],
  ["not an absolute path", "保护路径要写绝对路径。"],
  ["is not protected", "这个应用已经不在保护列表里，刷新后再试。"],
  [
    "has no usable protected path",
    "这个应用没有可用的保护路径，检查 desktop 文件或手动添加路径。",
  ],
  ["desktop file not found", "找不到这个应用的 desktop 文件。"],
  ["invalid app id", "应用 id 不合法。"],
  ["no home directory", "读不到这个账号的家目录。"],
  ["unknown user", "读不到这个账号的资料。"],
  ["serviceunknown", "应用锁没有响应，稍后再试。"],
  ["was not provided", "应用锁没有响应，稍后再试。"],
  ["connection refused", "应用锁没有响应，稍后再试。"],
];

export function readable(text) {
  const hay = text.toLowerCase();
  const hit = knownErrors.find(([needle]) => hay.includes(needle));
  return hit ? hit[1] : text;
}
