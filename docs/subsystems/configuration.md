# 配置与接口

## 配置

配置由 root 拥有，普通用户不能直接写。每个用户一个文件，`/etc/shisa/users/<用户名>.toml`：

```toml
[global]
grace_seconds = 300
clear_on_screen_lock = true
prompt_timeout_seconds = 60

[[apps]]
id = "firefox-nightly"
paths = ["/usr/lib/firefox-nightly/firefox"]
grace_seconds = 600
autostart = "prompt"
relock_after_unfocus_seconds = 0   # 0 关闭，1 立即，见[窗口管理](window-management.md)
```

`paths` 由 5.2 的解析自动填写，示例里的路径只是示意。应用 id 在配置、D-Bus 和 CLI 里写法一致，都不带 `.desktop` 后缀。

取值范围：`grace_seconds` 最大 31536000（一年），`prompt_timeout_seconds` 在 5 到 85 之间，受保护的应用至少有一条 `paths`。`SetOption` 收到范围外的值直接拒绝，不弹对话框。root 手改的配置在加载时把宽限期截到上限、把等待上限截进范围。宽限期设上限是因为解锁凭证的到期时间是认证时刻加宽限期，过大的值会让这个加法溢出。路径不能删空是因为没有路径的应用列在受保护里却什么都不拦，要放开应当走 `SetProtected`，对话框上写的才是“修改保护”。

## D-Bus

shisad 在 system bus 上，总线名 `io.github.RiverOnVenus.Shisa`，对象 `/io/github/RiverOnVenus/Shisa/Manager`。调用方的身份取自总线连接的凭据，每个用户只能看到和修改自己的配置。

```
ListApps() -> a(sbasas)              # 应用 id、是否有解锁凭证、配置路径、实际生效路径
ListInstalled() -> a(sss)            # 已安装应用：id、名称、图标名
AppConfig(app_id: s) -> (tbbt)        # 宽限期、是否允许自启动、是否受保护、切回重锁秒数
GlobalConfig() -> (tbt)              # 全局宽限期、锁屏是否清理解锁凭证、等待上限
ResolveApp(app_id: s) -> (asas)      # 从 desktop 文件算出的保护路径和覆盖说明，只读
SetProtected(app_id: s, on: b) -> ()
SetOption(app_id: s, key: s, value: v) -> ()
Lock(app_id: s) -> ()                # 空字符串表示全部
Unlock(app_id: s) -> ()              # 认证后发解锁凭证，只对受保护的应用有效
RegisterAgent() -> ()                # 接收 StateChanged 的会话代理
WindowStates() -> as                 # 窗口已放行的应用，见[窗口管理](window-management.md)
RelockSeconds() -> a(su)             # 每个应用的失焦隐藏秒数，关闭的不返回
signal StateChanged(app_id: s, unlocked: b)   # 只发给该用户的代理，不广播
signal SessionUnlocked()             # 解锁后发给该用户的代理，窗口脚本重新装载
RecentLaunches() -> (a(txsisxx)ttt) # 调用者最近的启动决策和累计次数，只读
Diagnostics() -> (sxxxttsb)         # 版本、启动时间、最近重新加载、最近加标记失败、受保护应用数、已标记路径数、配置文件、密码是否已设
```

`StateChanged` 在解锁凭证发放、`Lock`、锁屏清除、一次性凭证被启动用掉和凭证过期时发送。设置改动成功后也发一次，让会话代理立刻拿到新的重锁秒数。

`GlobalConfig` 读该用户的全局设置，没有配置文件时返回默认值 300 秒、锁屏清理解锁凭证、等待上限 60 秒。设置界面用它显示当前值，改动仍走 `SetOption` 并单独认证。

`RecentLaunches` 供通用页的诊断记录和应用详情使用。每行是 id、unix 毫秒、应用 id、pid、结果、等待截止时间（unix 毫秒，不等待时为 0）、回答时间（unix 毫秒，等待中为 0）。结果有 `ticket`、`instance`、`autostart`、`selfexec`（直接放行）、`asked`（等待认证）、`granted`、`timeout`、`cancelled`、`failed`、`denied`。`asked` 在认证有结果后改写成后五种之一。`denied` 是 polkit 明确返回未授权，用户取消和密码重试后放弃都落在这里。`failed` 是检查本身出错。`timeout` 是等待上限到点，`cancelled` 是检查在等待上限之后才返回。只返回调用者自己的记录，每个用户最多 64 条，按时间先后排列，累计次数从 shisad 启动算起，重启后清空。它只读，不调用 polkit。

`Diagnostics` 给设置界面的诊断卡片用。`started`、`reloaded` 和 `failed` 是 unix 毫秒，0 表示还没发生。`failed` 只在最近一次加载没有把路径标全时非零，随后一次完整加载会把它清零。应用数和路径数是调用者自己的策略，配置文件是 `/etc/shisa/users/<用户名>.toml`，还没创建时也会写出来。最后一个布尔值表示应用锁账号有没有密码：账号由 sysusers 建成锁定状态，root 没跑过 `passwd` 之前 polkit 的每次认证都会失败，设置界面据此在顶部提示。读 `/etc/shadow` 只看目标账号的密码字段，读不到时报告已就绪，避免把能用的系统误导到终端。它只读，不调用 polkit。

`ListApps` 的第三个字段是配置里的路径，第四个是上次加载时实际加上标记的路径。手写配置里的失效路径只出现在第三个字段，设置界面把它标成「未生效」，`shisa status` 也会注明。

`SetProtected`、`SetOption` 和 `Unlock` 会调用 polkit。启动请求的 subject 是挂起 exec 的调用方进程，这三个方法的 subject 是调用方的唯一总线名。polkit 按该用户的活动图形会话找到认证代理。没有代理时（TTY、SSH）调用失败，终端里的 `shisa unlock` 会自己启动 `pkttyagent`。

`SetProtected` 和 `SetOption` 会改变保护范围，每次调用都走一遍 polkit 认证，通过才生效，手里有解锁凭证也一样。否则旁边的人在宽限期内打开设置界面取消勾选就绕过去了。对话框上写明这次认证是为了打开哪个应用还是修改哪个应用。`SetOption` 的应用 id 为空时改的是全局选项，认证方式相同。改一个应用的宽限期会清掉它现有的解锁凭证，下一次启动按新宽限期重新认证，已经打开的窗口不受影响。用户还没有配置文件时，第一次认证成功后的改动会创建 `/etc/shisa/users/<用户名>.toml`，权限 0644，属主 root。`Lock` 只会收紧，不需要认证。

`WindowStates` 返回该用户窗口已放行的应用，和解锁凭证不是同一个集合。`RelockSeconds` 返回每个应用的失焦隐藏秒数，关闭的不返回。窗口层不调用 polkit，也不保存自己的凭证：它只读这两个方法，按结果隐藏或显示窗口。

会话代理在 session bus 上提供窗口同步接口 `io.github.RiverOnVenus.Shisa.Window`，对象 `/io/github/RiverOnVenus/Shisa/Window`，方法 `WindowEvent(app_id: s, pid: s, event: s) -> b`。KWin 脚本在受保护窗口失去焦点时发 `unfocused`，重新获得焦点时发 `focused`，装载完成时发 `loaded`。代理按前两个调整重锁计时，`loaded` 只用来确认脚本真的执行了。它不转给 shisad，也不认证。pid 是十进制字符串，原因见[窗口管理](window-management.md)。同一接口还提供 `WindowState`、`HiddenApps`、`RestoreWindow`、`WindowsChecked`、`HiddenWindows` 和 `VerifyLock`。窗口同步和激活的行为见[窗口管理](window-management.md)。

用 polkit 的取舍是 faillock：它的 PAM 栈走 `system-auth`。默认的管理员身份是 `shisa-lock`，所以计数落在应用锁账号上，不锁登录。应用锁密码和登录密码分开，知道登录密码的人如果没有应用锁密码，打不开受保护应用。

## CLI

```
shisa status
shisa lock [<app-id>]
shisa unlock <app-id>
shisa protect <app-id>
shisa unprotect <app-id>
shisa resolve <app-id>          # 只读，显示算出的保护路径和覆盖说明
```

## 验证

配置变更和 D-Bus 权限相关检查见[测试与验收](../testing.md)。
