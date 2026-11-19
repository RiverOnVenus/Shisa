//! Authorization through polkit (DESIGN 6).
//!
//! polkit 127 and 128 abort on a `unix-session` subject (polkit #699), so
//! every check names a process or a bus name instead. For a process outside
//! any session, such as a child of `user@.service`, polkit falls back to the
//! user's graphical session, which is how the desktop agent gets the prompt.

use crate::events::{Event, Events};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Instant;
use tracing::{debug, warn};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::Value;

const AUTHORITY: &str = "org.freedesktop.PolicyKit1";
const PATH: &str = "/org/freedesktop/PolicyKit1/Authority";
const IFACE: &str = "org.freedesktop.PolicyKit1.Authority";

pub enum Subject {
    /// The calling process, by pid and start time so a reused pid cannot be
    /// substituted.
    Process { pid: u32, start: u64 },
    /// A client on the system bus, by its unique name.
    BusName(String),
}

/// Ask polkit to authorize `action` for `uid`, prompting the user if allowed.
/// The outcome arrives as an [`Event`] for `request`. The check is cancelled
/// when `deadline` passes.
pub fn spawn(
    events: Arc<Events>,
    request: u64,
    uid: u32,
    app: String,
    action: &'static str,
    subject: Subject,
    deadline: Instant,
) {
    let on_failure = events.clone();
    let spawned = thread::Builder::new().name("polkit".into()).spawn(move || {
        check(events, request, uid, app, action, subject, deadline);
    });
    if spawned.is_err() {
        on_failure.push(Event::Failed {
            request,
            why: "cannot start authentication",
        });
    }
}

fn check(
    events: Arc<Events>,
    request: u64,
    uid: u32,
    app: String,
    action: &'static str,
    subject: Subject,
    deadline: Instant,
) {
    let conn = match Connection::system() {
        Ok(conn) => conn,
        Err(e) => {
            warn!(request, "polkit: no system bus: {e}");
            events.push(Event::Failed {
                request,
                why: "polkit unavailable",
            });
            return;
        }
    };
    // A cancellation id belongs to the connection that started the check, so
    // the timer cancels over a clone of that same connection.
    let cancellation = format!("shisa-{request}");
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    {
        let conn = conn.clone();
        let id = cancellation.clone();
        thread::spawn(move || {
            let left = deadline.saturating_duration_since(Instant::now());
            match stop_rx.recv_timeout(left) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    debug!(request, "cancelling polkit check");
                    if let Ok(proxy) = Proxy::new(&conn, AUTHORITY, PATH, IFACE) {
                        let result: zbus::Result<()> = proxy
                            .call_method("CancelCheckAuthorization", &(id,))
                            .map(|_| ());
                        if let Err(e) = result {
                            debug!(request, "polkit cancel: {e}");
                        }
                    }
                }
            }
        });
    }

    let name = if app.is_empty() {
        // Global settings have no app id.
        "Shisa 设置".to_string()
    } else {
        shisa_core::desktop::Env::for_user(uid)
            .map(|env| shisa_core::desktop::display_name(&app, &env))
            .unwrap_or_else(|| app.clone())
    };
    let details: HashMap<&str, &str> =
        HashMap::from([("app", name.as_str()), ("app_id", app.as_str())]);
    let subject: (&str, HashMap<&str, Value<'static>>) = match subject {
        Subject::Process { pid, start } => (
            "unix-process",
            HashMap::from([
                ("pid", Value::from(pid)),
                ("start-time", Value::from(start)),
            ]),
        ),
        Subject::BusName(name) => (
            "system-bus-name",
            HashMap::from([("name", Value::from(name))]),
        ),
    };
    let flags: u32 = 1; // AllowUserInteraction
    let result = (|| -> zbus::Result<zbus::Message> {
        let proxy = Proxy::new(&conn, AUTHORITY, PATH, IFACE)?;
        proxy.call_method(
            "CheckAuthorization",
            &(subject, action, details, flags, cancellation.as_str()),
        )
    })();
    // Stop the timer so it does not keep a thread and the connection alive.
    let _ = stop_tx.send(());

    let verdict = match result {
        Ok(reply) => reply
            .body()
            .deserialize::<(bool, bool, HashMap<String, String>)>()
            .map(|(allowed, _, _)| allowed)
            .map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    // The wait limit is a hard cutoff. An answer that arrives after it must
    // not release a caller the backup sweep may have denied already.
    if Instant::now() >= deadline {
        events.push(Event::Cancelled { request });
        return;
    }
    match verdict {
        Ok(true) => events.push(Event::Granted {
            uid,
            app,
            request,
        }),
        // A clean no is the user cancelling or failing to authenticate. It is
        // not an error, so it is recorded as a refusal rather than a failure.
        Ok(false) => events.push(Event::Denied { request }),
        Err(e) => {
            warn!(request, "polkit check: {e}");
            events.push(Event::Failed {
                request,
                why: "polkit error",
            });
        }
    }
}
