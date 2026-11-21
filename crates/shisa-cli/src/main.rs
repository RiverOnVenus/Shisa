//! shisa: command line client.
//!
//! `unlock` asks shisad for a ticket over the system bus. shisad asks polkit,
//! which shows the desktop's authentication dialog or, through the
//! `pkttyagent` this program starts, prompts in the terminal. `protect` and
//! `unprotect` authenticate the same way. `status` and `lock` go through the
//! system bus without authentication (docs/subsystems/configuration.md).

use shisa_core::dbus::ManagerProxyBlocking;
use std::time::Duration;
use std::{env, process};

fn usage() -> ! {
    eprintln!("usage: shisa unlock <app-id>");
    eprintln!("       shisa status");
    eprintln!("       shisa lock [<app-id>]");
    eprintln!("       shisa resolve <app-id>");
    eprintln!("       shisa protect <app-id>");
    eprintln!("       shisa unprotect <app-id>");
    process::exit(2);
}

fn main() {
    let mut args: Vec<std::ffi::OsString> = env::args_os().skip(1).collect();
    let command = args.first().map(|c| c.to_string_lossy().into_owned());
    let code = match command.as_deref() {
        Some("unlock") => unlock_command(args.split_off(1)),
        Some("status") => status(),
        Some("lock") => lock(args.split_off(1)),
        Some("resolve") => resolve(args.split_off(1)),
        Some("protect") => protect(args.split_off(1), true),
        Some("unprotect") => protect(args.split_off(1), false),
        _ => usage(),
    };
    process::exit(code);
}

fn manager() -> Result<ManagerProxyBlocking<'static>, String> {
    let conn = zbus::blocking::Connection::system()
        .map_err(|e| format!("cannot reach the system bus: {e}"))?;
    ManagerProxyBlocking::new(&conn).map_err(|e| format!("cannot reach shisad: {e}"))
}

/// `pkttyagent` registers an authentication agent for this process, so polkit
/// can prompt in the terminal when no desktop agent is there. Killed on drop.
struct PtyAgent(Option<process::Child>);

impl PtyAgent {
    fn start() -> Self {
        // Without a controlling terminal pkttyagent cannot prompt and prints
        // an error. Leave the check to the desktop agent in that case.
        let tty = unsafe {
            libc::open(c"/dev/tty".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOCTTY)
        };
        if tty < 0 {
            return PtyAgent(None);
        }
        unsafe { libc::close(tty) };
        let child = process::Command::new("pkttyagent")
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

fn status() -> i32 {
    let manager = match manager() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("shisa: {e}");
            return 1;
        }
    };
    match manager.list_apps() {
        Ok(apps) if apps.is_empty() => {
            println!("no protected apps for this user");
            0
        }
        Ok(apps) => {
            for (id, unlocked, paths, effective) in apps {
                println!("{:<4} {id}", if unlocked { "open" } else { "lock" });
                for path in paths {
                    if effective.contains(&path) {
                        println!("       {path}");
                    } else {
                        println!("       {path}  (未生效)");
                    }
                }
            }
            0
        }
        Err(e) => {
            eprintln!("shisa: {e}");
            1
        }
    }
}

/// Protect or unprotect an app. shisad authenticates the change through
/// polkit before it writes it (docs/subsystems/configuration.md).
fn protect(args: Vec<std::ffi::OsString>, on: bool) -> i32 {
    let app = match args.as_slice() {
        [one] => one.to_string_lossy().into_owned(),
        _ => usage(),
    };
    let _agent = PtyAgent::start();
    let manager = match manager() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("shisa: {e}");
            return 1;
        }
    };
    match manager.set_protected(&app, on) {
        Ok(()) => {
            println!("{}", if on { format!("{app} is now protected") } else { format!("{app} is no longer protected") });
            0
        }
        Err(e) => {
            eprintln!("shisa: {e}");
            eprintln!("认证没有完成。图形会话里会弹出系统的认证框，终端里由 pkttyagent 提示。");
            1
        }
    }
}

/// Show the protected paths of an installed application (docs/subsystems/launch-gate.md). The
/// daemon resolves them with the user's session environment, the same one
/// protection uses. The local scan is only a fallback when the daemon is not
/// available.
fn resolve(args: Vec<std::ffi::OsString>) -> i32 {
    let app = match args.as_slice() {
        [one] => one.to_string_lossy().into_owned(),
        _ => usage(),
    };
    let env = shisa_core::desktop::Env::current();
    if let Ok(manager) = manager() {
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
                    println!("  注意：{note}");
                }
                return 0;
            }
            // An older daemon without the method, or no daemon at all: the
            // local scan is the best answer left.
            Err(e) if unavailable(&e) => {}
            Err(e) => {
                eprintln!("shisa: {app}: {e}");
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
                println!("  注意：{}", note.message());
            }
            0
        }
        Err(e) => {
            eprintln!("shisa: {app}: {e}");
            1
        }
    }
}

/// The daemon could not answer this method: it is not running, or it is too
/// old to know it. Either way the local scan can stand in.
fn unavailable(e: &zbus::Error) -> bool {
    matches!(e, zbus::Error::MethodError(name, _, _)
        if matches!(
            name.as_str(),
            "org.freedesktop.DBus.Error.UnknownMethod"
                | "org.freedesktop.DBus.Error.UnknownInterface"
                | "org.freedesktop.DBus.Error.ServiceUnknown"
                | "org.freedesktop.DBus.Error.NameHasNoOwner"
        ))
}

fn lock(args: Vec<std::ffi::OsString>) -> i32 {
    let app = match args.as_slice() {
        [] => String::new(),
        [one] => one.to_string_lossy().into_owned(),
        _ => usage(),
    };
    let manager = match manager() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("shisa: {e}");
            return 1;
        }
    };
    match manager.lock(&app) {
        Ok(()) => {
            if app.is_empty() {
                println!("all tickets dropped");
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
                        eprintln!("窗口状态未确认：{e}");
                    }
                }
            } else {
                println!("{app} locked");
            }
            0
        }
        Err(e) => {
            eprintln!("shisa: {e}");
            1
        }
    }
}

fn unlock_command(args: Vec<std::ffi::OsString>) -> i32 {
    let app = match args.as_slice() {
        [one] => one.to_string_lossy().into_owned(),
        _ => usage(),
    };
    unlock_dbus(&app)
}

fn unlock_dbus(app: &str) -> i32 {
    let _agent = PtyAgent::start();
    let manager = match manager() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("shisa: {e}");
            return 1;
        }
    };
    match manager.unlock(app) {
        Ok(()) => {
            println!("{app} unlocked.");
            0
        }
        Err(e) => {
            eprintln!("shisa: {e}");
            1
        }
    }
}
