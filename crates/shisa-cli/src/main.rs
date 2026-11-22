//! shisa: command line client.
//!
//! `unlock` asks shisad for a ticket over the system bus. shisad asks polkit,
//! which shows the desktop's authentication dialog or, through the
//! `pkttyagent` this program starts, prompts in the terminal. `protect` and
//! `unprotect` authenticate the same way. `status` and `lock` go through the
//! system bus without authentication (docs/subsystems/configuration.md).

use shisa_core::dbus::ManagerProxyBlocking;
use shisa_core::i18n::{self, Language};
use std::time::Duration;
use std::{env, process};

fn usage(language: Language) -> ! {
    eprintln!("{}", text(language, "用法：", "usage:"));
    eprintln!("       shisa unlock <app-id>");
    eprintln!("       shisa status");
    eprintln!("       shisa lock [<app-id>]");
    eprintln!("       shisa resolve <app-id>");
    eprintln!("       shisa protect <app-id>");
    eprintln!("       shisa unprotect <app-id>");
    process::exit(2);
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
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
        [one] => Some(one.to_string_lossy().into_owned()),
        _ => None,
    }
}

/// Parse the command line. `None` means the arguments do not match any
/// command and the caller prints the usage.
fn parse_command(args: &[std::ffi::OsString]) -> Option<Command> {
    let (command, rest) = args.split_first()?;
    Some(match command.to_string_lossy().as_ref() {
        "unlock" => Command::Unlock(one_arg(rest)?),
        "status" if rest.is_empty() => Command::Status,
        "lock" => match rest {
            [] => Command::Lock(String::new()),
            [one] => Command::Lock(one.to_string_lossy().into_owned()),
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
        Some(Command::Unlock(app)) => unlock_command(app, language),
        Some(Command::Status) => status(language),
        Some(Command::Lock(app)) => lock(app, language),
        Some(Command::Resolve(app)) => resolve(app, language),
        Some(Command::Protect { app, on }) => protect(app, on, language),
        None => usage(language),
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
        let child = process::Command::new("/usr/bin/pkttyagent")
            .arg("--process")
            .arg(process::id().to_string())
            .spawn()
            .ok();
        // Give it a moment to register before the first check. A desktop
        // agent is the fallback when this fails.
        if child.is_some() {
            std::thread::sleep(Duration::from_millis(200));
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
        ];
    }
    let mut lines = Vec::new();
    for (id, unlocked, paths, effective) in apps {
        let state = match (unlocked, language) {
            (true, Language::ZhCn) => "解锁",
            (false, Language::ZhCn) => "锁定",
            (true, Language::En) => "open",
            (false, Language::En) => "lock",
        };
        // `{:<4}` pads by characters, and the two Chinese words are already
        // two display columns each, so padding them would misalign the id.
        lines.push(match language {
            Language::ZhCn => format!("{state} {id}"),
            Language::En => format!("{state:<4} {id}"),
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
    match manager.set_protected(&app, on) {
        Ok(()) => {
            println!(
                "{}",
                match (on, language) {
                    (true, Language::ZhCn) => format!("{app} 已设为受保护"),
                    (false, Language::ZhCn) => format!("{app} 已取消保护"),
                    (true, Language::En) => format!("{app} is now protected"),
                    (false, Language::En) => format!("{app} is no longer protected"),
                }
            );
            0
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
                    text(language, "已收回全部票据", "all tickets dropped")
                );
                if env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some() {
                    let check = (|| -> zbus::Result<()> {
                        use shisa_core::dbus::{WINDOW_BUS_NAME, WINDOW_INTERFACE, WINDOW_PATH};
                        let conn = zbus::blocking::Connection::session()?;
                        let proxy = zbus::blocking::Proxy::new(
                            &conn,
                            WINDOW_BUS_NAME,
                            WINDOW_PATH,
                            WINDOW_INTERFACE,
                        )?;
                        proxy.call("VerifyLock", &())
                    })();
                    if let Err(e) = check {
                        eprintln!(
                            "{}: {e}",
                            text(language, "窗口状态未确认", "window state not confirmed")
                        );
                    }
                }
            } else {
                println!(
                    "{}",
                    match language {
                        Language::ZhCn => format!("{app} 已上锁"),
                        Language::En => format!("{app} locked"),
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
                    Language::ZhCn => format!("{app} 已解锁。"),
                    Language::En => format!("{app} unlocked."),
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
                "lock app",
                "       /usr/bin/app",
                "       /opt/missing  (inactive)",
                "open other",
                "       /usr/bin/other",
            ]
        );
        assert_eq!(
            status_lines(&apps, Language::ZhCn),
            vec![
                "锁定 app",
                "       /usr/bin/app",
                "       /opt/missing  (未生效)",
                "解锁 other",
                "       /usr/bin/other",
            ]
        );
        assert_eq!(
            status_lines(&[], Language::En),
            vec!["no protected apps for this user"]
        );
        assert_eq!(
            status_lines(&[], Language::ZhCn),
            vec!["这个用户没有受保护的应用"]
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
