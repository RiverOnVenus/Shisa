// Interface text for the settings and restore windows.
//
// The dictionaries are plain data so `npm test` can cover the rules in
// format.js without a browser. `createTranslator` returns a `t(key, params)`
// function. A template may contain `{name}` placeholders, and a value with a
// `|` offers singular and plural forms chosen by the `count` parameter.

export const languages = [
  ["zh-CN", "简体中文"],
  ["en", "English"],
];

const zh = {
  "app.title": "Shisa 设置",
  "restore.title": "恢复窗口",

  "common.seconds": "秒",
  "common.add": "添加",
  "common.remove": "移除",
  "common.close": "关闭",
  "common.notes": "说明",

  "filters.all": "全部",
  "filters.protected": "受保护",
  "filters.unlocked": "已认证",
  "sidebar.search": "搜索应用",
  "sidebar.group": "应用",
  "sidebar.empty.match": "没有匹配的应用",
  "sidebar.empty.none": "没有可显示的应用",
  "sidebar.empty.clear": "清除筛选",
  "sidebar.general": "通用",
  "daemon.online": "应用锁在运行",
  "daemon.offline": "应用锁没有响应",
  "agent.online": "会话代理在运行",
  "agent.offline": "会话代理没有运行",
  "agent.hint.on": "托盘和窗口隐藏由 shisa-agent 提供，窗口隐藏需要 KWin。",
  "agent.hint.off":
    "shisa-agent 没有运行，托盘和窗口隐藏都不可用。运行 systemctl --user start shisa-agent.service。",
  "health.ok": "运行正常",
  "health.daemonOff": "应用锁无响应",
  "health.agentOff": "会话代理未运行",
  "health.bothOff": "应用锁和会话代理都未运行",
  "health.password": "应用锁密码未设置",
  "health.version": "shisad 版本不一致",
  "health.hint.password": "设置应用锁密码：sudo passwd shisa-lock",
  "health.hint.version":
    "shisad 与设置程序版本不一致，重启 shisad 和 shisa-agent 后再打开设置。",
  "dot.unlocked": "已认证，有效期内启动和恢复不再询问",
  "dot.locked": "需认证，启动和恢复需要应用锁密码",

  "crumb.settings": "设置",
  "crumb.apps": "应用",
  "crumb.general": "通用",
  "crumb.activity": "最近启动",
  "top.search": "查找设置",
  "top.lockAll": "全部上锁",
  "top.lockAllNone": "没有受保护的应用",
  "top.lockAllHint": "全部上锁。设置窗口会关闭，以结束本次授权。",
  "top.minimize": "最小化",
  "top.close": "关闭",

  "busy.auth": "等待认证…",
  "busy.lock": "正在上锁…",

  "password.name": "还没设置应用锁密码",
  "password.sub":
    "受保护的应用要输入应用锁密码，这个密码还没设，现在所有认证都会失败。请管理员在终端运行",
  "password.after": "，完成后点「重新检查」。",
  "password.recheck": "重新检查",
  "password.ready": "应用锁密码已设置。",
  "password.missing": "应用锁密码仍未设置。",
  "version.name": "shisad 与设置程序设计版本不一致",
  "version.sub":
    "shisad {daemon}，设置程序 {app}。升级后请运行 sudo systemctl restart shisad.service，并运行 systemctl --user restart shisa-agent.service。",

  "general.grace.name": "默认宽限期",
  "general.grace.sub": "认证通过后这段时间内再打开不再询问。",
  "general.clear.name": "锁屏时收回认证",
  "general.clear.sub": "开启后锁屏收回受保护窗口，解锁后重新认证。",
  "general.clear.note": "关闭时，锁屏不改变正在使用的窗口，只影响下一次启动。",
  "general.timeout.name": "认证等待上限",
  "general.timeout.sub":
    "超时即拒绝启动。连续 3 次认证失败会暂停应用锁约 10 分钟。",
  "general.timeout.note":
    "取消和超时都算一次认证失败，暂停只影响应用锁，不影响登录。需要立即解除时运行",
  "general.timeout.after": "。",
  "general.timeout.unit": "秒 · 5 到 85",
  "general.custom": "自定义",
  "general.language.name": "语言",

  "diag.view": "查看最近启动和拒绝原因",
  "diag.title": "诊断",
  "diag.details": "详细信息",
  "diag.uptime": "已运行 {duration}，最近一次重新加载{reloaded}。",
  "diag.reloaded.at": "在 {ago}",
  "diag.reloaded.never": "还没发生",
  "diag.apps.name": "受保护的应用",
  "diag.apps.sub": "{apps} 个应用，{marked} 条路径已加上监视标记。",
  "diag.last.failed": "有路径没标记上，{ago}",
  "diag.last.ok": "全部路径已标记。",
  "diag.configFailed.sub":
    "上一次配置没有读出或解析（{ago}），仍在用旧的保护策略。",
  "diag.config": "配置文件",
  "diag.unavailable": "当前守护进程不提供诊断信息，装上新版 shisad 后可用。",
  "settings.empty.match": "没有匹配的设置",
  "settings.empty.clear": "清除搜索",
  "settings.loading": "正在读取设置…",

  "activity.title": "最近启动",
  "activity.all": "全部",
  "activity.denied": "只看拒绝",
  "activity.summary":
    "本次运行 {requests} 次启动，{allowed} 次放行，{denied} 次拒绝。记录只保存在 shisad 的内存里，重启后清空。",
  "activity.note.old": "当前守护进程不提供活动记录，装上新版 shisad 后可用。",
  "activity.note.offline": "应用锁没有响应，等它恢复后这一页会自动更新。",
  "activity.empty.none":
    "还没有启动记录。打开一个受保护的应用，它的裁决会出现在这里。",
  "activity.empty.denied": "没有拒绝的记录。",
  "activity.open": "打开这个应用的页面",

  "app.state.unlocked": "已认证",
  "app.state.locked": "需认证",
  "app.lockNow": "收回认证",
  "app.unlockNow": "现在认证",
  "app.failure": "最近一次启动失败",
  "app.protect.name": "启动前要求认证",
  "app.protect.sub": "开启后，启动这个应用要输入应用锁密码。",
  "app.protect.nopath": "没有可拦截的启动入口，无法开启保护。",
  "app.inactive.count": "{count} 条保护路径未生效，启动拦截不完整。",
  "app.grace.name": "宽限期",
  "app.relock.name": "失焦后隐藏",
  "app.relock.sub": "失焦这么久后隐藏窗口。宽限期内恢复不再询问。",
  "app.relock.warn":
    "失焦隐藏保留启动凭证，宽限期内可免密还原。借出桌面前请先「全部上锁」。",
  "app.advanced": "高级设置",
  "app.customRelock": "自定义失焦时长",
  "app.autostart.name": "自启动",
  "app.autostart.sub": "登录后 30 秒内的自启动直接放行。",
  "app.autostart.warn":
    "单元名同账号可以伪造，只在你信任自己登录后第一分钟时使用。",
  "app.autostart.prompt": "要求认证",
  "app.autostart.allow": "放行",
  "app.path.inactive":
    "未生效，当前没有被监视。检查文件是否存在，或者它是不是 shell、解释器。",
  "app.path.keep":
    "至少保留一条保护路径。要放开这个应用，请关掉上面的保护开关。",
  "app.path.none": "还没有路径。",
  "app.path.placeholder": "/usr/bin/程序 或 /opt/游戏/启动器",
  "app.coverage.name": "保护范围",
  "app.coverage.note": "保护范围列出启动会拦截的路径，以及窗口隐藏的实际边界。",
  "app.paths.name": "启动入口",
  "app.paths.potential": "开启保护后会拦截这些路径。",
  "app.paths.noneEffective": "当前没有路径在生效。",
  "app.windows.name": "已打开的窗口",
  "app.windows.sub": "锁屏后隐藏，重开时重新认证。",
  "app.windows.note": "窗口层以用户身份运行，同账号的程序可以绕过隐藏和恢复。",
  "app.path.inactivePrefix": "未生效：",
  "app.coverage.empty": "没有从 desktop 文件解析到可保护的入口。",
  "app.none": "选择左侧的应用",
  "app.addPath.absolute": "保护路径要写绝对路径，例如 /usr/bin/程序。",

  "toast.locked": "{app} 的认证已收回",
  "toast.authorized": "{app} 已认证",

  "restore.windows": "{count} 个窗口",
  "restore.ticketed": "点击恢复，不需要密码 · {count} 个窗口",
  "restore.auth": "点击恢复，需要应用锁密码 · {count} 个窗口",
  "restore.loading": "正在查找窗口…",
  "restore.empty": "没有隐藏的窗口",
  "restore.agentMissing": "会话代理没有运行",
  "restore.agentMissing.sub":
    "托盘和窗口隐藏都不可用。在终端运行 systemctl --user start shisa-agent.service，然后重试。",
  "restore.retry": "重试",
  "restore.openSettings": "打开设置",
  "restore.settings": "设置",
  "restore.lockAll": "全部上锁",
  "restore.lockedAll": "已全部上锁。",
  "restore.restored": "已恢复，窗口马上回来。",
  "restore.waiting": "正在恢复…",
  "restore.error.read": "无法读取隐藏窗口。",
  "restore.error.restore": "无法恢复窗口，请重试。",
  "restore.error.lock": "无法上锁，请重试。",
  "restore.error.settings": "无法打开设置。",

  "grace.every": "每次",
  "grace.5m": "5 分钟",
  "grace.30m": "30 分钟",
  "grace.1h": "1 小时",
  "relock.off": "关闭",
  "relock.now": "立即",
  "relock.1m": "1 分钟",
  "relock.5m": "5 分钟",
  "relock.30m": "30 分钟",

  "outcome.granted": "认证通过",
  "outcome.ticket": "已认证",
  "outcome.instance": "已在运行",
  "outcome.autostart": "自启动",
  "outcome.selfexec": "子进程",
  "outcome.asked": "认证中",
  "outcome.timeout": "超时",
  "outcome.failed": "出错",
  "outcome.denied": "未通过",
  "outcome.allow": "放行",
  "outcome.wait": "等待",
  "outcome.refuse": "拒绝",

  "ago.justNow": "刚刚",
  "ago.seconds": "{count} 秒前",
  "ago.minutes": "{count} 分钟前",
  "ago.hours": "{count} 小时前",
  "duration.seconds": "{count} 秒",
  "duration.minutes": "{count} 分钟",
  "duration.hours": "{count} 小时",
  "duration.days": "{count} 天",
  "detail.waiting": "已等 {count} 秒",
  "detail.authenticated": "认证用时 {count} 秒",
};

const en = {
  "app.title": "Shisa Settings",
  "restore.title": "Restore windows",

  "common.seconds": "seconds",
  "common.add": "Add",
  "common.remove": "Remove",
  "common.close": "Close",
  "common.notes": "Notes",

  "filters.all": "All",
  "filters.protected": "Protected",
  "filters.unlocked": "Authorized",
  "sidebar.search": "Search apps",
  "sidebar.group": "Apps",
  "sidebar.empty.match": "No matching apps",
  "sidebar.empty.none": "No apps to show",
  "sidebar.empty.clear": "Clear filters",
  "sidebar.general": "General",
  "daemon.online": "App lock is running",
  "daemon.offline": "App lock is not responding",
  "agent.online": "Session agent is running",
  "agent.offline": "Session agent is not running",
  "agent.hint.on":
    "shisa-agent provides the tray and window hiding, which needs KWin.",
  "agent.hint.off":
    "shisa-agent is not running, so the tray and window hiding are unavailable. Run systemctl --user start shisa-agent.service.",
  "health.ok": "Running normally",
  "health.daemonOff": "App lock is not responding",
  "health.agentOff": "Session agent is not running",
  "health.bothOff": "Both services are down",
  "health.password": "App-lock password is not set",
  "health.version": "shisad version differs",
  "health.hint.password":
    "Set the app-lock password: translate passwd shisa-lock",
  "health.hint.version":
    "shisad and the settings app differ. Restart shisad and shisa-agent, then reopen settings.",
  "dot.unlocked": "Authorized. Launches and restores do not ask while valid.",
  "dot.locked": "Authentication required for launches and restores.",

  "crumb.settings": "Settings",
  "crumb.apps": "Apps",
  "crumb.general": "General",
  "crumb.activity": "Recent launches",
  "top.search": "Search settings",
  "top.lockAll": "Lock all",
  "top.lockAllNone": "No protected apps",
  "top.lockAllHint":
    "Locks all apps. This window closes to end the current authorization.",
  "top.minimize": "Minimize",
  "top.close": "Close",

  "busy.auth": "Waiting for authentication…",
  "busy.lock": "Locking…",

  "password.name": "The app-lock password is not set",
  "password.sub":
    "Protected apps ask for the app-lock password, which has not been set, so every authentication will fail. Ask an administrator to run",
  "password.after": " in a terminal, then choose Check again.",
  "password.recheck": "Check again",
  "password.ready": "The app-lock password is set.",
  "password.missing": "The app-lock password is still not set.",
  "version.name": "shisad and the settings app have different versions",
  "version.sub":
    "shisad {daemon}, settings {app}. After an upgrade, run sudo systemctl restart shisad.service and systemctl --user restart shisa-agent.service.",

  "general.grace.name": "Default grace period",
  "general.grace.sub":
    "After a successful authentication, launches in this period do not ask again.",
  "general.clear.name": "Revoke authorization on screen lock",
  "general.clear.sub":
    "Takes back protected windows when the screen locks. Unlocking asks again.",
  "general.clear.note":
    "When off, a screen lock leaves windows in use alone and only affects the next launch.",
  "general.timeout.name": "Authentication timeout",
  "general.timeout.sub":
    "Timing out refuses the launch. Three failures in a row pause the app lock for about 10 minutes.",
  "general.timeout.note":
    "Cancelling and timing out both count as a failure. The pause affects only the app lock, not login. To clear it immediately, run",
  "general.timeout.after": ".",
  "general.timeout.unit": "seconds · 5 to 85",
  "general.custom": "Custom",
  "general.language.name": "Language",

  "diag.view": "View recent launches and refusals",
  "diag.title": "Diagnostics",
  "diag.details": "Details",
  "diag.uptime": "Running for {duration}, last reload {reloaded}.",
  "diag.reloaded.at": "{ago}",
  "diag.reloaded.never": "has not happened",
  "diag.apps.name": "Protected apps",
  "diag.apps.sub":
    "{apps} {appWord}, {marked} {pathWord} marked for monitoring.",
  "diag.last.failed": "Some paths could not be marked, {ago}",
  "diag.last.ok": "All paths are marked.",
  "diag.configFailed.sub":
    "The last configuration could not be read or parsed, {ago}. The previous policy is still in effect.",
  "diag.config": "Configuration file",
  "diag.unavailable":
    "This daemon does not provide diagnostics. Install a newer shisad to see them.",
  "settings.empty.match": "No matching settings",
  "settings.empty.clear": "Clear search",
  "settings.loading": "Reading settings…",

  "activity.title": "Recent launches",
  "activity.all": "All",
  "activity.denied": "Refusals only",
  "activity.summary":
    "This run: {requests} {launchWord}, {allowed} allowed, {denied} refused. Records live only in shisad's memory and are cleared by a restart.",
  "activity.note.old":
    "This daemon does not provide launch records. Install a newer shisad to see them.",
  "activity.note.offline":
    "The app lock is not responding. This page updates automatically when it returns.",
  "activity.empty.none":
    "No launches yet. Open a protected app and its decision appears here.",
  "activity.empty.denied": "No refusals.",
  "activity.open": "Open this app's page",

  "app.state.unlocked": "Authorized",
  "app.state.locked": "Authentication required",
  "app.lockNow": "Revoke",
  "app.unlockNow": "Authenticate now",
  "app.failure": "Last launch failed",
  "app.protect.name": "Require authentication before launch",
  "app.protect.sub":
    "When on, launching this app asks for the app-lock password.",
  "app.protect.nopath":
    "No launch entry can be gated, so protection cannot be turned on.",
  "app.inactive.count":
    "{count} protected path does not take effect, so launch gating is incomplete.|{count} protected paths do not take effect, so launch gating is incomplete.",
  "app.grace.name": "Grace period",
  "app.relock.name": "Hide when focus is lost",
  "app.relock.sub":
    "Hides the window after this long without focus. Restoring in the grace period does not ask again.",
  "app.relock.warn":
    "A focus-loss hide keeps the launch ticket, so restores stay password-free until the grace period ends. Use Lock all before lending the desktop.",
  "app.advanced": "Advanced settings",
  "app.customRelock": "Custom focus-loss delay",
  "app.autostart.name": "Autostart",
  "app.autostart.sub":
    "A launch from the autostart unit within 30 seconds of login is allowed without a prompt.",
  "app.autostart.warn":
    "The unit name can be forged by the same account. Use this only if you trust the first minute after your login.",
  "app.autostart.prompt": "Ask for authentication",
  "app.autostart.allow": "Allow",
  "app.path.inactive":
    "Inactive and not monitored. Check that the file exists and is not a shell or interpreter.",
  "app.path.keep":
    "Keep at least one protected path. To release this app, turn off protection above.",
  "app.path.none": "No paths yet.",
  "app.path.placeholder": "/usr/bin/program or /opt/game/launcher",
  "app.coverage.name": "Protection coverage",
  "app.coverage.note":
    "Protection coverage lists the paths that gate launches and the limits of window hiding.",
  "app.paths.name": "Launch paths",
  "app.paths.potential": "Turning on protection gates these paths.",
  "app.paths.noneEffective": "No path is in effect right now.",
  "app.windows.name": "Open windows",
  "app.windows.sub": "Hidden after a lock, and asks again when reopened.",
  "app.windows.note":
    "The window layer runs as the user, so programs under the same account can bypass hiding and restoring.",
  "app.path.inactivePrefix": "Inactive: ",
  "app.coverage.empty":
    "No protectable entry was resolved from the desktop file.",
  "app.none": "Select an app on the left",
  "app.addPath.absolute":
    "Protected paths must be absolute, for example /usr/bin/program.",

  "toast.locked": "Revoked authorization for {app}",
  "toast.authorized": "{app} is authorized",

  "restore.windows": "{count} window|{count} windows",
  "restore.ticketed":
    "Click to restore without a password · {count} window|Click to restore without a password · {count} windows",
  "restore.auth":
    "Click to restore, asks for the app-lock password · {count} window|Click to restore, asks for the app-lock password · {count} windows",
  "restore.loading": "Looking for windows…",
  "restore.empty": "No hidden windows",
  "restore.agentMissing": "The session agent is not running",
  "restore.agentMissing.sub":
    "The tray and window hiding are unavailable. Run systemctl --user start shisa-agent.service in a terminal, then retry.",
  "restore.retry": "Retry",
  "restore.openSettings": "Open settings",
  "restore.settings": "Settings",
  "restore.lockAll": "Lock all",
  "restore.lockedAll": "Locked all apps.",
  "restore.restored": "Restored. The window is coming back.",
  "restore.waiting": "Restoring…",
  "restore.error.read": "Could not read hidden windows.",
  "restore.error.restore": "Could not restore the window. Try again.",
  "restore.error.lock": "Could not lock. Try again.",
  "restore.error.settings": "Could not open settings.",

  "grace.every": "Every time",
  "grace.5m": "5 minutes",
  "grace.30m": "30 minutes",
  "grace.1h": "1 hour",
  "relock.off": "Off",
  "relock.now": "Immediately",
  "relock.1m": "1 minute",
  "relock.5m": "5 minutes",
  "relock.30m": "30 minutes",

  "outcome.granted": "authenticated",
  "outcome.ticket": "authorized",
  "outcome.instance": "already running",
  "outcome.autostart": "autostart",
  "outcome.selfexec": "child process",
  "outcome.asked": "authenticating",
  "outcome.timeout": "timed out",
  "outcome.failed": "failed",
  "outcome.denied": "not authorized",
  "outcome.allow": "allow",
  "outcome.wait": "wait",
  "outcome.refuse": "deny",

  "ago.justNow": "just now",
  "ago.seconds": "{count}s ago",
  "ago.minutes": "{count} min ago",
  "ago.hours": "{count}h ago",
  "duration.seconds": "{count}s",
  "duration.minutes": "{count} min",
  "duration.hours": "{count}h",
  "duration.days": "{count}d",
  "detail.waiting": "waited {count}s",
  "detail.authenticated": "answered in {count}s",
};

export const messages = { "zh-CN": zh, en };

// Tags the frontend receives from the shell. Anything else is English.
export function languageFromTag(tag) {
  return String(tag ?? "")
    .toLowerCase()
    .startsWith("zh")
    ? "zh-CN"
    : "en";
}

export function createTranslator(language) {
  const dict = messages[language] ?? messages.en;
  return (key, values = {}) => {
    let template = dict[key] ?? messages["zh-CN"][key] ?? key;
    if (template.includes("|") && "count" in values) {
      const forms = template.split("|");
      template =
        Number(values.count) === 1 ? forms[0] : forms[forms.length - 1];
    }
    return template.replace(/\{(\w+)\}/g, (_, name) =>
      name in values ? String(values[name]) : `{${name}}`,
    );
  };
}

// The word for a count, for the few sentences that name two counts.
export function plural(language, count, one, other) {
  return language === "en" && Number(count) === 1 ? one : other;
}
