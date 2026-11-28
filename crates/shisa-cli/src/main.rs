//! shisa: command line client.
//!
//! `unlock` asks shisad for a ticket over the system bus. shisad asks polkit,
//! which shows the desktop's authentication dialog or, through the
//! `pkttyagent` this program starts, prompts in the terminal. `protect` and
//! `unprotect` authenticate the same way. `status` and `lock` go through the
//! system bus without authentication (docs/subsystems/configuration.md).

use shisa_core::dbus::{AppRow, LockReport, ManagerProxyBlocking};
use shisa_core::i18n::{self, Language, lock_report_text};
use std::time::Duration;
use std::{env, process};

/// The command list, shared by `--help` and the invalid-usage path. An app id
/// is the desktop file name without `.desktop`; saying so here saves a trip to
/// the settings window.
fn help_lines(language: Language) -> Vec<String> {
    match language {
        Language::ZhCn => vec![
            "用法：".into(),
            "  shisa status             查看受保护的应用和认证状态".into(),
            "  shisa lock [<app-id>]    收回全部或一个应用的认证".into(),
            "  shisa unlock <app-id>    现在认证一个应用，宽限期内启动不再询问".into(),
            "  shisa protect <app-id>   保护一个应用（需要应用锁密码）".into(),
            "  shisa unprotect <app-id> 取消保护".into(),
            "  shisa resolve <app-id>   查看 desktop 文件解析出的启动入口".into(),
            "".into(),
            "app-id 是 desktop 文件名去掉 .desktop，例如 org.kde.dolphin。".into(),
            "用 shisa status 或设置窗口查看受保护的应用。".into(),
        ],
        Language::En => vec![
            "usage:".into(),
            "  shisa status             show protected apps and authorization".into(),
            "  shisa lock [<app-id>]    revoke every authorization, or one app's".into(),
            "  shisa unlock <app-id>    authenticate now and skip the prompt inside the grace period".into(),
            "  shisa protect <app-id>   protect an app (asks for the app-lock password)".into(),
            "  shisa unprotect <app-id> stop protecting an app".into(),
            "  shisa resolve <app-id>   show the launch entries a desktop file resolves to".into(),
            "".into(),
            "app-id is the desktop file name without .desktop, for example org.kde.dolphin.".into(),
            "Use shisa status or Shisa Settings to see protected apps.".into(),
        ],
    }
}

fn print_help(language: Language, to_stdout: bool) {
    for line in help_lines(language) {
        if to_stdout {
            println!("{line}");
        } else {
            eprintln!("{line}");
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Unlock(String),
    Status,
    /// An empty app id locks every app of the caller.
    Lock(String),
    Resolve(String),
    Protect {
        app: String,
        on: bool,
    },
}

/// One argument, or `None` when the command took a different number.
fn one_arg(args: &[std::ffi::OsString]) -> Option<String> {
    match args {
        [one] if !one.is_empty() => one.to_str().map(str::to_owned),
        _ => None,
    }
}

/// Parse the command line. `None` means the arguments do not match any
/// command and the caller prints the usage.
fn parse_command(args: &[std::ffi::OsString]) -> Option<Command> {
    let (command, rest) = args.split_first()?;
    Some(match command.to_string_lossy().as_ref() {
        "help" | "--help" | "-h" => Command::Help,
        "unlock" => Command::Unlock(one_arg(rest)?),
        "status" if rest.is_empty() => Command::Status,
        "lock" => match rest {
            [] => Command::Lock(String::new()),
            [_] => Command::Lock(one_arg(rest)?),
            _ => return None,
        },
        "resolve" => Command::Resolve(one_arg(rest)?),
        "protect" => Command::Protect {
            app: one_arg(rest)?,
            on: true,
        },
        "unprotect" => Command::Protect {
            app: one_arg(rest)?,
            on: false,
        },
        _ => return None,
    })
}

fn main() {
    let args: Vec<std::ffi::OsString> = env::args_os().skip(1).collect();
    let language = i18n::current();
    let code = match parse_command(&args) {
        Some(Command::Help) => {
            print_help(language, true);
            0
        }
        Some(Command::Unlock(app)) => unlock_command(app, language),
        Some(Command::Status) => status(language),
        Some(Command::Lock(app)) => lock(app, language),
        Some(Command::Resolve(app)) => resolve(app, language),
        Some(Command::Protect { app, on }) => protect(app, on, language),
        None => {
            print_help(language, false);
            2
        }
    };
    process::exit(code);
}

/// Pick between the two texts for the interface language.
fn text(language: Language, zh: &'static str, en: &'static str) -> &'static str {
    match language {
        Language::ZhCn => zh,
        Language::En => en,
    }
}

/// The daemon's own errors are English. Show them in the caller's language.
fn shown(error: impl std::fmt::Display, language: Language) -> String {
    i18n::error_text(&error.to_string(), language)
}

fn manager(language: Language) -> Result<ManagerProxyBlocking<'static>, String> {
    let conn = zbus::blocking::Connection::system().map_err(|e| match language {
        Language::ZhCn => format!("连不上系统总线：{e}"),
        Language::En => format!("cannot reach the system bus: {e}"),
    })?;
    ManagerProxyBlocking::new(&conn).map_err(|e| match language {
        Language::ZhCn => format!("连不上 shisad：{e}"),
        Language::En => format!("cannot reach shisad: {e}"),
    })
}

/// `pkttyagent` registers an authentication agent for this process, so polkit
/// can prompt in the terminal when no desktop agent is there. Killed on drop.
struct PtyAgent(Option<process::Child>);

impl PtyAgent {
    fn start() -> Self {
        // Without a controlling terminal pkttyagent cannot prompt and prints
        // an error. Leave the check to the desktop agent in that case.
        let tty = unsafe {
            libc::open(
                c"/dev/tty".as_ptr(),
                libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOCTTY,
            )
        };
        if tty < 0 {
            return PtyAgent(None);
        }
        unsafe { libc::close(tty) };
        // The absolute path keeps a user-writable directory earlier in PATH
        // from supplying the authentication helper. Arch, Fedora, and Debian
        // install polkit's helper at this path.
        use std::io::Read;
        use std::os::fd::AsRawFd;
        use std::os::unix::{net::UnixStream, process::CommandExt};

        let Ok((mut ready, notify)) = UnixStream::pair() else {
            return PtyAgent(None);
        };
        if ready
            .set_read_timeout(Some(Duration::from_secs(5)))
            .is_err()
        {
            return PtyAgent(None);
        }
        let fd = notify.as_raw_fd();
        let mut command = process::Command::new("/usr/bin/pkttyagent");
        command
            .arg("--process")
            .arg(process::id().to_string())
            .arg("--notify-fd")
            .arg(fd.to_string());
        // SAFETY: fcntl is async-signal-safe and notify owns fd through spawn.
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().ok();
        drop(notify);
        // pkttyagent closes the descriptor after registering or on exit.
        if child.is_some() {
            let _ = ready.read(&mut [0]);
        }
        PtyAgent(child)
    }
}

impl Drop for PtyAgent {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn status(language: Language) -> i32 {
    let manager = match manager(language) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("shisa: {e}");
            return 1;
        }
    };
    match manager.list_apps() {
        Ok(apps) => {
            for line in status_lines(&apps, language) {
                println!("{line}");
            }
            0
        }
        Err(e) => {
            eprintln!("shisa: {}", shown(e, language));
            1
        }
    }
}

/// Lines `shisa status` prints. A path without a mark says so, because
/// settings would otherwise show protection that does not hold.
fn status_lines(apps: &[shisa_core::dbus::AppRow], language: Language) -> Vec<String> {
    if apps.is_empty() {
        return vec![
            text(
                language,
                "这个用户没有受保护的应用",
                "no protected apps for this user",
            )
            .into(),
            text(
                language,
                "用 shisa protect <app-id> 或设置窗口添加保护。",
                "Use shisa protect <app-id> or Shisa Settings to add protection.",
            )
            .into(),
        ];
    }
    let mut lines = Vec::new();
    for (id, unlocked, paths, effective) in apps {
        let state = match (unlocked, language) {
            (true, Language::ZhCn) => "已认证",
            (false, Language::ZhCn) => "需认证",
            (true, Language::En) => "authorized",
            (false, Language::En) => "locked",
        };
        // `{:<10}` keeps the two English states in one column. The Chinese
        // labels are not padded, because padding them by characters would
        // misalign the id.
        lines.push(match language {
            Language::ZhCn => format!("{state} {id}"),
            Language::En => format!("{state:<10} {id}"),
        });
        for path in paths {
            if effective.contains(path) {
                lines.push(format!("       {path}"));
            } else {
                lines.push(format!(
                    "       {path}  ({})",
                    text(language, "未生效", "inactive")
                ));
            }
        }
    }
    lines
}

/// How much of a just-saved protection is actually in effect. A path that
/// could not be marked stays in the config and is listed as inactive, so the
/// command must report that instead of a success the user cannot rely on.
#[derive(Debug, PartialEq, Eq)]
enum Effect {
    /// Every configured path is marked.
    Full,
    /// At least one path is marked, these are not.
    Partial(Vec<String>),
    /// No configured path is marked.
    Unmarked(Vec<String>),
    /// The daemon answered the change but does not list the app.
    Missing,
}

fn effect_from_rows(apps: &[AppRow], app: &str) -> Effect {
    let Some((_, _, paths, effective)) = apps.iter().find(|row| row.0 == app) else {
        return Effect::Missing;
    };
    let missing: Vec<String> = paths
        .iter()
        .filter(|path| !effective.contains(path))
        .cloned()
        .collect();
    if effective.is_empty() {
        Effect::Unmarked(missing)
    } else if missing.is_empty() {
        Effect::Full
    } else {
        Effect::Partial(missing)
    }
}

fn app_effect(manager: &ManagerProxyBlocking<'_>, app: &str) -> Result<Effect, String> {
    Ok(effect_from_rows(
        &manager.list_apps().map_err(|e| e.to_string())?,
        app,
    ))
}

/// Protect or unprotect an app. shisad authenticates the change through
/// polkit before it writes it (docs/subsystems/configuration.md).
fn protect(app: String, on: bool, language: Language) -> i32 {
    let _agent = PtyAgent::start();
    let manager = match manager(language) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("shisa: {e}");
            return 1;
        }
    };
    let success = match (on, language) {
        (true, Language::ZhCn) => format!("{app} 已设为受保护"),
        (false, Language::ZhCn) => format!("{app} 已取消保护"),
        (true, Language::En) => format!("{app} is now protected"),
        (false, Language::En) => format!("{app} is no longer protected"),
    };
    match manager.set_protected(&app, on) {
        Ok(()) => {
            if !on {
                println!("{success}");
                return 0;
            }
            // The config is written before the daemon reports success. The
            // caller has to know whether the paths it names are marked.
            match app_effect(&manager, &app) {
                Ok(Effect::Full) => {
                    println!("{success}");
                    0
                }
                Ok(Effect::Partial(missing)) => {
                    println!("{success}");
                    eprintln!(
                        "{}",
                        text(
                            language,
                            "以下路径没有生效，其余路径已受保护：",
                            "These paths are not in effect, the rest are protected:"
                        )
                    );
                    for path in missing {
                        eprintln!("  {path}");
                    }
                    0
                }
                Ok(Effect::Unmarked(missing)) => {
                    eprintln!(
                        "{}",
                        match language {
                            Language::ZhCn => format!("{app} 已保存为受保护，但没有任何路径生效。"),
                            Language::En =>
                                format!("{app} was saved as protected, but no path is in effect."),
                        }
                    );
                    for path in missing {
                        eprintln!("  {path}");
                    }
                    eprintln!(
                        "{}",
                        text(
                            language,
                            "检查路径是否存在并且是普通文件，或用 shisa resolve 查看可用的启动路径。",
                            "Check that the paths exist and are regular files, or use shisa resolve to see the launch entries."
                        )
                    );
                    1
                }
                Ok(Effect::Missing) => {
                    eprintln!(
                        "{}",
                        text(
                            language,
                            "改动已保存，但 shisad 没有列出这个应用。",
                            "The change was saved, but shisad does not list the app."
                        )
                    );
                    1
                }
                Err(e) => {
                    println!("{success}");
                    eprintln!(
                        "{}",
                        text(
                            language,
                            "改动已保存，但无法确认保护路径是否生效：",
                            "The change was saved, but the protected paths could not be checked:"
                        )
                    );
                    eprintln!("shisa: {e}");
                    1
                }
            }
        }
        Err(e) => {
            eprintln!("shisa: {}", shown(e, language));
            eprintln!(
                "{}",
                text(
                    language,
                    "认证没有完成。图形会话里会弹出系统的认证框，终端里由 pkttyagent 提示。",
                    "Authentication did not complete. A desktop session shows the system prompt, and a terminal gets it from pkttyagent.",
                )
            );
            1
        }
    }
}

/// Show the protected paths of an installed application (docs/subsystems/launch-gate.md). The
/// daemon resolves them with the user's session environment, the same one
/// protection uses. The local scan is only a fallback when the daemon is not
/// available.
fn resolve(app: String, language: Language) -> i32 {
    let env = shisa_core::desktop::Env::current();
    if let Ok(manager) = manager(language) {
        match manager.resolve_app(&app) {
            Ok((paths, notes)) => {
                let name = shisa_core::desktop::display_name(&app, &env);
                if name == app {
                    println!("{app}");
                } else {
                    println!("{app}  {name}");
                }
                for path in &paths {
                    println!("  {path}");
                }
                for note in &notes {
                    println!("  {} {note}", text(language, "注意：", "note:"));
                }
                return 0;
            }
            // An older daemon without the method, or no daemon at all: the
            // local scan is the best answer left.
            Err(e) if unavailable(&e) => {}
            Err(e) => {
                eprintln!("shisa: {app}: {}", shown(e, language));
                return 1;
            }
        }
    }
    match shisa_core::desktop::resolve(&app, &env) {
        Ok(found) => {
            println!("{}  {}", app, found.name);
            println!("  {}", found.desktop_file.display());
            for path in &found.paths {
                println!("  {}", path.display());
            }
            for note in &found.notes {
                println!(
                    "  {} {}",
                    text(language, "注意：", "note:"),
                    note.message(language)
                );
            }
            0
        }
        Err(e) => {
            eprintln!("shisa: {app}: {}", shown(e, language));
            1
        }
    }
}

/// The daemon could not answer this method: it is not running, or it is too
/// old to know it. Either way the local scan can stand in.
fn unavailable(e: &zbus::Error) -> bool {
    match e {
        zbus::Error::MethodError(name, _, _) => unavailable_name(name.as_str()),
        _ => false,
    }
}

fn unavailable_name(name: &str) -> bool {
    matches!(
        name,
        "org.freedesktop.DBus.Error.UnknownMethod"
            | "org.freedesktop.DBus.Error.UnknownInterface"
            | "org.freedesktop.DBus.Error.ServiceUnknown"
            | "org.freedesktop.DBus.Error.NameHasNoOwner"
    )
}

fn lock(app: String, language: Language) -> i32 {
    let manager = match manager(language) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("shisa: {e}");
            return 1;
        }
    };
    match manager.lock(&app) {
        Ok(()) => {
            if app.is_empty() {
                println!(
                    "{}",
                    text(language, "已收回全部认证", "all authorizations revoked")
                );
                if env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some() {
                    match verify_lock_report() {
                        Ok(report) => println!("{}", lock_report_text(&report, language)),
                        Err(e) => {
                            // Only an older or missing agent gets the legacy
                            // call. A timeout means the agent is busy or
                            // stuck, and a second check would add another
                            // notification and delay.
                            if unavailable(&e) {
                                let _ = verify_lock_legacy();
                            }
                            eprintln!(
                                "{}: {e}",
                                text(language, "窗口状态未确认", "window state not confirmed")
                            );
                        }
                    }
                }
            } else {
                println!(
                    "{}",
                    match language {
                        Language::ZhCn => format!("{app} 的认证已收回"),
                        Language::En => format!("authorization for {app} revoked"),
                    }
                );
            }
            0
        }
        Err(e) => {
            eprintln!("shisa: {}", shown(e, language));
            1
        }
    }
}

fn verify_lock_report() -> zbus::Result<LockReport> {
    use shisa_core::dbus::{WINDOW_BUS_NAME, WINDOW_INTERFACE, WINDOW_PATH};
    let conn = zbus::blocking::connection::Builder::session()?
        .method_timeout(Duration::from_secs(5))
        .build()?;
    let proxy = zbus::blocking::Proxy::new(&conn, WINDOW_BUS_NAME, WINDOW_PATH, WINDOW_INTERFACE)?;
    proxy.call("VerifyLockReport", &())
}

fn verify_lock_legacy() -> zbus::Result<()> {
    use shisa_core::dbus::{WINDOW_BUS_NAME, WINDOW_INTERFACE, WINDOW_PATH};
    let conn = zbus::blocking::connection::Builder::session()?
        .method_timeout(Duration::from_secs(5))
        .build()?;
    let proxy = zbus::blocking::Proxy::new(&conn, WINDOW_BUS_NAME, WINDOW_PATH, WINDOW_INTERFACE)?;
    proxy.call("VerifyLock", &())
}

fn unlock_command(app: String, language: Language) -> i32 {
    unlock_dbus(&app, language)
}

fn unlock_dbus(app: &str, language: Language) -> i32 {
    let _agent = PtyAgent::start();
    let manager = match manager(language) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("shisa: {e}");
            return 1;
        }
    };
    match manager.unlock(app) {
        Ok(()) => {
            println!(
                "{}",
                match language {
                    Language::ZhCn => format!("{app} 已认证。"),
                    Language::En => format!("{app} authorized."),
                }
            );
            0
        }
        Err(e) => {
            eprintln!("shisa: {}", shown(e, language));
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn args(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }

    #[test]
    fn invalid_ids_do_not_select_an_app_or_global_lock() {
        use std::os::unix::ffi::OsStringExt;
        for command in ["lock", "unlock", "protect", "unprotect", "resolve"] {
            assert_eq!(parse_command(&args(&[command, ""])), None);
            assert_eq!(
                parse_command(&[command.into(), OsString::from_vec(vec![0xff])]),
                None
            );
        }
    }

    #[test]
    fn each_command_takes_its_own_arity() {
        assert_eq!(
            parse_command(&args(&["unlock", "app"])),
            Some(Command::Unlock("app".into()))
        );
        assert_eq!(parse_command(&args(&["unlock"])), None);
        assert_eq!(parse_command(&args(&["unlock", "a", "b"])), None);

        assert_eq!(parse_command(&args(&["status"])), Some(Command::Status));
        assert_eq!(parse_command(&args(&["status", "x"])), None);

        assert_eq!(
            parse_command(&args(&["lock"])),
            Some(Command::Lock(String::new()))
        );
        assert_eq!(
            parse_command(&args(&["lock", "app"])),
            Some(Command::Lock("app".into()))
        );
        assert_eq!(parse_command(&args(&["lock", "a", "b"])), None);

        assert_eq!(
            parse_command(&args(&["resolve", "app"])),
            Some(Command::Resolve("app".into()))
        );
        assert_eq!(parse_command(&args(&["resolve"])), None);

        assert_eq!(
            parse_command(&args(&["protect", "app"])),
            Some(Command::Protect {
                app: "app".into(),
                on: true
            })
        );
        assert_eq!(
            parse_command(&args(&["unprotect", "app"])),
            Some(Command::Protect {
                app: "app".into(),
                on: false
            })
        );
        assert_eq!(parse_command(&args(&["unprotect"])), None);

        assert_eq!(parse_command(&args(&["--help"])), Some(Command::Help));
        assert_eq!(parse_command(&args(&["-h"])), Some(Command::Help));
        assert_eq!(parse_command(&args(&["help"])), Some(Command::Help));

        assert_eq!(parse_command(&args(&[])), None);
        assert_eq!(parse_command(&args(&["nonsense"])), None);
    }

    #[test]
    fn status_marks_paths_without_a_mark() {
        let apps = vec![
            (
                "app".into(),
                false,
                vec!["/usr/bin/app".into(), "/opt/missing".into()],
                vec!["/usr/bin/app".into()],
            ),
            (
                "other".into(),
                true,
                vec!["/usr/bin/other".into()],
                vec!["/usr/bin/other".into()],
            ),
        ];
        assert_eq!(
            status_lines(&apps, Language::En),
            vec![
                "locked     app",
                "       /usr/bin/app",
                "       /opt/missing  (inactive)",
                "authorized other",
                "       /usr/bin/other",
            ]
        );
        assert_eq!(
            status_lines(&apps, Language::ZhCn),
            vec![
                "需认证 app",
                "       /usr/bin/app",
                "       /opt/missing  (未生效)",
                "已认证 other",
                "       /usr/bin/other",
            ]
        );
        assert_eq!(
            status_lines(&[], Language::En),
            vec![
                "no protected apps for this user",
                "Use shisa protect <app-id> or Shisa Settings to add protection.",
            ]
        );
        assert_eq!(
            status_lines(&[], Language::ZhCn),
            vec![
                "这个用户没有受保护的应用",
                "用 shisa protect <app-id> 或设置窗口添加保护。",
            ]
        );
    }

    #[test]
    fn a_saved_protection_reports_paths_that_did_not_take_effect() {
        let rows: Vec<AppRow> = vec![
            (
                "full".into(),
                false,
                vec!["/usr/bin/app".into()],
                vec!["/usr/bin/app".into()],
            ),
            (
                "partial".into(),
                false,
                vec!["/usr/bin/app".into(), "/opt/missing".into()],
                vec!["/usr/bin/app".into()],
            ),
            ("none".into(), false, vec!["/opt/missing".into()], vec![]),
        ];
        assert_eq!(effect_from_rows(&rows, "full"), Effect::Full);
        assert_eq!(
            effect_from_rows(&rows, "partial"),
            Effect::Partial(vec!["/opt/missing".into()])
        );
        assert_eq!(
            effect_from_rows(&rows, "none"),
            Effect::Unmarked(vec!["/opt/missing".into()])
        );
        assert_eq!(effect_from_rows(&rows, "missing"), Effect::Missing);
    }

    #[test]
    fn a_lock_report_names_the_window_result() {
        use shisa_core::dbus::{LOCK_CONFIRMED, LOCK_NO_LAYER, LOCK_PARTIAL, LOCK_UNCONFIRMED};

        let report =
            |status: u8, matched: u64, hidden: u64, missed: Vec<(&str, u32)>| -> LockReport {
                (
                    status,
                    1,
                    matched,
                    hidden,
                    missed
                        .into_iter()
                        .map(|(app, count)| (app.to_string(), count))
                        .collect(),
                )
            };
        assert_eq!(
            lock_report_text(&report(LOCK_CONFIRMED, 2, 2, vec![]), Language::ZhCn),
            "启动已上锁，已收起 2/2 个受保护窗口"
        );
        assert_eq!(
            lock_report_text(&report(LOCK_CONFIRMED, 1, 1, vec![]), Language::En),
            "Lock applied. Hid 1 of 1 protected window"
        );
        assert_eq!(
            lock_report_text(&report(LOCK_CONFIRMED, 0, 0, vec![]), Language::En),
            "Lock applied. No protected window matched on this desktop"
        );
        let partial = report(LOCK_PARTIAL, 3, 2, vec![("shisa.test.missing", 1)]);
        assert!(lock_report_text(&partial, Language::ZhCn).contains("未确认：shisa.test.missing"));
        assert_eq!(
            lock_report_text(&report(LOCK_UNCONFIRMED, 0, 0, vec![]), Language::En),
            "Lock applied, but the window state was not confirmed"
        );
        assert_eq!(
            lock_report_text(&report(LOCK_NO_LAYER, 0, 0, vec![]), Language::ZhCn),
            "启动已上锁，本桌面没有窗口层，未能收起窗口"
        );
        assert_eq!(
            lock_report_text(&report(99, 0, 0, vec![]), Language::ZhCn),
            "启动已上锁，窗口状态未确认"
        );
        assert_eq!(
            lock_report_text(&report(LOCK_PARTIAL, 2, 1, vec![]), Language::ZhCn),
            "启动已上锁，窗口状态未确认"
        );
        assert_eq!(
            lock_report_text(&report(LOCK_CONFIRMED, 2, 1, vec![]), Language::ZhCn),
            "启动已上锁，窗口状态未确认"
        );
    }

    #[test]
    fn only_a_missing_method_or_service_falls_back_to_the_local_scan() {
        for name in [
            "org.freedesktop.DBus.Error.UnknownMethod",
            "org.freedesktop.DBus.Error.UnknownInterface",
            "org.freedesktop.DBus.Error.ServiceUnknown",
            "org.freedesktop.DBus.Error.NameHasNoOwner",
        ] {
            assert!(unavailable_name(name), "{name}");
        }
        assert!(!unavailable_name("org.freedesktop.DBus.Error.AccessDenied"));
        assert!(!unavailable_name(""));
    }
}
