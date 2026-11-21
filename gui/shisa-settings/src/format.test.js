import test from "node:test";
import assert from "node:assert/strict";
import {
  ago,
  detail,
  duration,
  isPreset,
  isRelockPreset,
  kindText,
  matches,
  outcomeClass,
  readable,
  reasonText,
  seconds,
} from "./format.js";

test("ago picks the unit that reads best", () => {
  const now = 10_000_000;
  assert.equal(ago(now, now), "刚刚");
  assert.equal(ago(now - 4_000, now), "刚刚");
  assert.equal(ago(now - 5_000, now), "5 秒前");
  assert.equal(ago(now - 59_000, now), "59 秒前");
  assert.equal(ago(now - 60_000, now), "1 分钟前");
  assert.equal(ago(now - 3_540_000, now), "59 分钟前");
  assert.equal(ago(now - 3_600_000, now), "1 小时前");
  assert.equal(ago(now - 7_200_000, now), "2 小时前");
  // A timestamp in the future is not shown as a negative age.
  assert.equal(ago(now + 60_000, now), "刚刚");
});

test("seconds clamps and rounds", () => {
  assert.equal(seconds(-5), 0);
  assert.equal(seconds(499), 0);
  assert.equal(seconds(500), 1);
  assert.equal(seconds(1_499), 1);
  assert.equal(seconds(1_500), 2);
});

test("duration switches unit at 60 and 48 hours", () => {
  assert.equal(duration(0), "0 秒");
  assert.equal(duration(59_000), "59 秒");
  assert.equal(duration(60_000), "1 分钟");
  assert.equal(duration(3_540_000), "59 分钟");
  assert.equal(duration(3_600_000), "1 小时");
  assert.equal(duration(47 * 3_600_000), "47 小时");
  assert.equal(duration(48 * 3_600_000), "2 天");
  assert.equal(duration(72 * 3_600_000), "3 天");
});

test("launch outcomes name the decision and its reason", () => {
  assert.equal(kindText("granted"), "放行");
  assert.equal(reasonText("granted"), "认证通过");
  assert.equal(kindText("asked"), "等待");
  assert.equal(outcomeClass("granted"), "ok");
  assert.equal(outcomeClass("denied"), "bad");
  assert.equal(outcomeClass("asked"), "wait");
  // An outcome a newer daemon sends is shown as it came, not swallowed.
  assert.equal(kindText("future-kind"), "future-kind");
  assert.equal(reasonText("future-kind"), "");
  assert.equal(outcomeClass("future-kind"), "wait");
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
    detail({ pid: 7, kind: "asked", at: now - 5_000 }, now),
    "pid 7 · 已等 5 秒",
  );
  assert.equal(
    detail({ pid: 7, kind: "granted", at: 1_000, resolved_at: 4_000 }, now),
    "pid 7 · 认证用时 3 秒",
  );
  // A zero answer time means the box was never answered.
  assert.equal(
    detail({ pid: 7, kind: "denied", at: 1_000, resolved_at: 0 }, now),
    "pid 7",
  );
  // A decision taken without a box has no answering time.
  assert.equal(
    detail({ pid: 7, kind: "ticket", at: 1_000, resolved_at: 4_000 }, now),
    "pid 7",
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
  assert.ok(!matches(app, "", "unprotected"));

  const locked = { id: "a", name: "A", protected: true, unlocked: false };
  assert.ok(!matches(locked, "", "unlocked"));
  assert.ok(!matches(locked, "", "unprotected"));

  const free = { id: "b", name: "B", protected: false, unlocked: false };
  assert.ok(matches(free, "", "unprotected"));
  assert.ok(!matches(free, "", "protected"));
});

test("readable translates known daemon errors and keeps the rest", () => {
  assert.equal(
    readable("authentication was rejected"),
    "认证没有通过，改动没有生效。",
  );
  // The match is case-insensitive because zbus messages vary in case.
  assert.equal(
    readable("ServiceUnknown: no owner"),
    "应用锁没有响应，稍后再试。",
  );
  assert.equal(readable("something new happened"), "something new happened");
});
