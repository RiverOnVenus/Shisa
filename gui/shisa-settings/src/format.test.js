import test from "node:test";
import assert from "node:assert/strict";
import {
  ago,
  detail,
  duration,
  isPreset,
  isRelockPreset,
  kindText,
  lockNotice,
  matches,
  outcomeClass,
  reasonText,
  seconds,
} from "./format.js";
import { createTranslator, languageFromTag } from "./i18n.js";

const zh = createTranslator("zh-CN");
const en = createTranslator("en");

test("ago picks the unit that reads best", () => {
  const now = 10_000_000;
  assert.equal(ago(now, now, zh), "刚刚");
  assert.equal(ago(now - 4_000, now, zh), "刚刚");
  assert.equal(ago(now - 5_000, now, zh), "5 秒前");
  assert.equal(ago(now - 59_000, now, zh), "59 秒前");
  assert.equal(ago(now - 60_000, now, zh), "1 分钟前");
  assert.equal(ago(now - 3_540_000, now, zh), "59 分钟前");
  assert.equal(ago(now - 3_600_000, now, zh), "1 小时前");
  assert.equal(ago(now - 7_200_000, now, zh), "2 小时前");
  // A timestamp in the future is not shown as a negative age.
  assert.equal(ago(now + 60_000, now, zh), "刚刚");

  assert.equal(ago(now, now, en), "just now");
  assert.equal(ago(now - 5_000, now, en), "5s ago");
  assert.equal(ago(now - 60_000, now, en), "1 min ago");
  assert.equal(ago(now - 3_600_000, now, en), "1h ago");
});

test("seconds clamps and rounds", () => {
  assert.equal(seconds(-5), 0);
  assert.equal(seconds(499), 0);
  assert.equal(seconds(500), 1);
  assert.equal(seconds(1_499), 1);
  assert.equal(seconds(1_500), 2);
});

test("duration switches unit at 60 and 48 hours", () => {
  assert.equal(duration(0, zh), "0 秒");
  assert.equal(duration(59_000, zh), "59 秒");
  assert.equal(duration(60_000, zh), "1 分钟");
  assert.equal(duration(3_540_000, zh), "59 分钟");
  assert.equal(duration(3_600_000, zh), "1 小时");
  assert.equal(duration(47 * 3_600_000, zh), "47 小时");
  assert.equal(duration(48 * 3_600_000, zh), "2 天");
  assert.equal(duration(72 * 3_600_000, zh), "3 天");

  assert.equal(duration(0, en), "0s");
  assert.equal(duration(60_000, en), "1 min");
  assert.equal(duration(3_600_000, en), "1h");
  assert.equal(duration(48 * 3_600_000, en), "2d");
});

test("launch outcomes name the decision and its reason", () => {
  assert.equal(kindText("granted", zh), "放行");
  assert.equal(reasonText("granted", zh), "认证通过");
  assert.equal(kindText("asked", zh), "等待");
  assert.equal(kindText("revoked", zh), "拒绝");
  assert.equal(reasonText("revoked", zh), "锁定在先");
  assert.equal(outcomeClass("granted"), "ok");
  assert.equal(outcomeClass("denied"), "bad");
  assert.equal(outcomeClass("revoked"), "bad");
  assert.equal(outcomeClass("asked"), "wait");
  // An outcome a newer daemon sends is shown as it came, not swallowed.
  assert.equal(kindText("future-kind", zh), "future-kind");
  assert.equal(reasonText("future-kind", zh), "");
  assert.equal(outcomeClass("future-kind"), "wait");

  assert.equal(kindText("granted", en), "allow");
  assert.equal(reasonText("granted", en), "authenticated");
  assert.equal(kindText("asked", en), "wait");
});

test("a lock notice reports the window check's result", () => {
  assert.equal(
    lockNotice({ status: "confirmed", matched: 2, hidden: 2, missed: [] }, zh),
    "启动已上锁，已收起 2/2 个受保护窗口。",
  );
  assert.equal(
    lockNotice({ status: "confirmed", matched: 0, hidden: 0, missed: [] }, zh),
    "启动已上锁，当前桌面没有匹配的受保护窗口。",
  );
  assert.equal(
    lockNotice(
      {
        status: "partial",
        matched: 3,
        hidden: 2,
        missed: [{ app: "org.kde.kate", count: 1 }],
      },
      zh,
      (id) => (id === "org.kde.kate" ? "Kate" : id),
    ),
    "启动已上锁，已收起 2/3 个受保护窗口，未确认：Kate。",
  );
  assert.equal(
    lockNotice({ status: "unconfirmed" }, zh),
    "启动已上锁，窗口状态未确认。",
  );
  assert.equal(
    lockNotice({ status: "no-window-layer" }, zh),
    "启动已上锁，本桌面没有窗口层，未能收起窗口。",
  );
  // Name the first three apps and count the rest.
  const missed = ["a", "b", "c", "d"].map((app) => ({ app, count: 1 }));
  assert.equal(
    lockNotice({ status: "partial", matched: 5, hidden: 1, missed }, zh, (id) =>
      id.toUpperCase(),
    ),
    "启动已上锁，已收起 1/5 个受保护窗口，未确认：A、B、C 等 1 个应用。",
  );
  // A missing result, an unknown status, and a report without names are
  // never shown as a success.
  assert.equal(lockNotice(null, zh), "启动已上锁，窗口状态未确认。");
  assert.equal(
    lockNotice({ status: "future" }, zh),
    "启动已上锁，窗口状态未确认。",
  );
  assert.equal(
    lockNotice({ status: "confirmed" }, zh),
    "启动已上锁，窗口状态未确认。",
  );
  assert.equal(
    lockNotice({ status: "partial", matched: 2, hidden: 1, missed: [] }, zh),
    "启动已上锁，窗口状态未确认。",
  );

  assert.equal(
    lockNotice({ status: "confirmed", matched: 1, hidden: 1, missed: [] }, en),
    "Lock applied. Hid 1 of 1 protected window.",
  );
  assert.equal(
    lockNotice({ status: "confirmed", matched: 2, hidden: 2, missed: [] }, en),
    "Lock applied. Hid 2 of 2 protected windows.",
  );
  assert.equal(
    lockNotice(
      {
        status: "partial",
        matched: 2,
        hidden: 1,
        missed: [{ app: "a", count: 1 }],
      },
      en,
      (id) => id.toUpperCase(),
    ),
    "Lock applied. Hid 1 of 2 protected windows. Not confirmed: A.",
  );
  assert.equal(
    lockNotice({ status: "no-window-layer" }, en),
    "Lock applied. This desktop has no window layer, so windows were not hidden.",
  );
});

test("preset checks match the picker values", () => {
  assert.ok(isPreset(0));
  assert.ok(isPreset(300));
  assert.ok(isPreset(3600));
  assert.ok(!isPreset(42));
  assert.ok(isRelockPreset(1));
  assert.ok(isRelockPreset(1800));
  assert.ok(!isRelockPreset(2));
});

test("detail names the number behind a decision", () => {
  const now = 10_000_000;
  assert.equal(
    detail({ pid: 7, kind: "asked", at: now - 5_000 }, now, zh),
    "pid 7 · 已等 5 秒",
  );
  assert.equal(
    detail({ pid: 7, kind: "granted", at: 1_000, resolved_at: 4_000 }, now, zh),
    "pid 7 · 认证用时 3 秒",
  );
  // A zero answer time means the box was never answered.
  assert.equal(
    detail({ pid: 7, kind: "denied", at: 1_000, resolved_at: 0 }, now, zh),
    "pid 7",
  );
  // A decision taken without a box has no answering time.
  assert.equal(
    detail({ pid: 7, kind: "ticket", at: 1_000, resolved_at: 4_000 }, now, zh),
    "pid 7",
  );

  assert.equal(
    detail({ pid: 7, kind: "asked", at: now - 5_000 }, now, en),
    "pid 7 · waited 5s",
  );
  assert.equal(
    detail({ pid: 7, kind: "granted", at: 1_000, resolved_at: 4_000 }, now, en),
    "pid 7 · answered in 3s",
  );
});

test("matches searches the name and the id, then the filter", () => {
  const app = {
    id: "org.kde.kwrite",
    name: "Kate",
    protected: true,
    unlocked: true,
  };
  assert.ok(matches(app, "", "all"));
  assert.ok(matches(app, "  kate ", "all"));
  assert.ok(matches(app, "KWRITE", "all"));
  assert.ok(!matches(app, "firefox", "all"));
  assert.ok(matches(app, "", "protected"));
  assert.ok(matches(app, "", "unlocked"));

  const locked = { id: "a", name: "A", protected: true, unlocked: false };
  assert.ok(!matches(locked, "", "unlocked"));

  const free = { id: "b", name: "B", protected: false, unlocked: false };
  assert.ok(!matches(free, "", "protected"));
  assert.ok(!matches(free, "", "unlocked"));
});

test("translations fill parameters and choose the plural form", () => {
  assert.equal(zh("restore.windows", { count: 1 }), "1 个窗口");
  assert.equal(zh("restore.windows", { count: 3 }), "3 个窗口");
  assert.equal(en("restore.windows", { count: 1 }), "1 window");
  assert.equal(en("restore.windows", { count: 3 }), "3 windows");
  assert.equal(
    en("diag.uptime", { duration: "5 min", reloaded: "2 min ago" }),
    "Running for 5 min, last reload 2 min ago.",
  );
  // A missing key is visible instead of silently empty.
  assert.equal(zh("no.such.key"), "no.such.key");
});

test("language tags outside Chinese are English", () => {
  assert.equal(languageFromTag("zh-CN"), "zh-CN");
  assert.equal(languageFromTag("zh_TW"), "zh-CN");
  assert.equal(languageFromTag("en"), "en");
  assert.equal(languageFromTag("de-DE"), "en");
  assert.equal(languageFromTag(undefined), "en");
});
