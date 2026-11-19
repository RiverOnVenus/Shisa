//! Screen locking through logind (DESIGN 5.4, 13.5).
//!
//! Reads `LockedHint` and the start time of the users' graphical sessions,
//! the latter for the autostart window (5.5). The property is what the Plasma
//! lock screen sets, so this needs no help from the session agent.
//!
//! logind announces every change with a signal, and each signal triggers one
//! look at the sessions. A slow timer does the same in case a signal is ever
//! missed, so a lost signal delays the clearing instead of preventing it.

use crate::bus::{Msg, Shared};
use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::{debug, warn};
use zbus::MatchRule;
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::message::Type;
use zbus::zvariant::OwnedObjectPath;

/// Sweep interval while signals arrive, as a safety net.
const FALLBACK: Duration = Duration::from_secs(30);
/// Sweep interval when the signals could not be subscribed to.
const POLL: Duration = Duration::from_secs(2);
/// logind sends several signals for one change. Wait this long so they are
/// handled by one sweep.
const SETTLE: Duration = Duration::from_millis(150);

/// One active graphical session seen at the last look.
#[derive(Clone, Copy)]
struct SessionLock {
    uid: u32,
    locked: bool,
}

/// Active graphical sessions by logind object path. A user counts as locked
/// only when every session in here is locked.
type Sessions = HashMap<String, SessionLock>;

pub fn spawn(state: Shared, notify: Sender<Msg>) -> io::Result<()> {
    thread::Builder::new().name("logind".into()).spawn(move || poll(state, notify))?;
    Ok(())
}

fn poll(state: Shared, notify: Sender<Msg>) {
    let conn = match Connection::system() {
        Ok(conn) => conn,
        Err(e) => {
            warn!("logind: no system bus: {e}");
            return;
        }
    };
    let (wake_tx, wake_rx) = mpsc::channel::<()>();
    let subscribed = match signals(&conn) {
        Ok(stream) => thread::Builder::new()
            .name("logind-signals".into())
            .spawn(move || {
                for message in stream {
                    if message.is_err() || wake_tx.send(()).is_err() {
                        break;
                    }
                }
            })
            .is_ok(),
        Err(e) => {
            warn!("logind: cannot subscribe to signals, polling instead: {e}");
            false
        }
    };
    let mut interval = if subscribed { FALLBACK } else { POLL };
    // Sessions seen at the last look. A user counts as locked only when every
    // active graphical session is locked, so one locked session does not clear
    // the tickets of someone working in another one. The transition out of
    // that state is what tells the agent to reapply the window layer, because
    // KWin ignores window changes made while the screen is locked.
    let mut seen = Sessions::new();
    loop {
        if let Err(e) = sweep(&conn, &state, &notify, &mut seen) {
            debug!("logind: {e}");
        }
        match wake_rx.recv_timeout(interval) {
            Ok(()) => {
                thread::sleep(SETTLE);
                while wake_rx.try_recv().is_ok() {}
            }
            Err(RecvTimeoutError::Timeout) => {}
            // The signal thread ended: fall back to polling.
            Err(RecvTimeoutError::Disconnected) => {
                interval = POLL;
                thread::sleep(POLL);
            }
        }
    }
}

/// Every signal logind sends: sessions coming and going, and their property
/// changes, `LockedHint` among them.
fn signals(conn: &Connection) -> zbus::Result<MessageIterator> {
    let rule = MatchRule::builder().msg_type(Type::Signal).sender("org.freedesktop.login1")?.build();
    MessageIterator::for_match_rule(rule, conn, None)
}

fn sweep(
    conn: &Connection,
    state: &Shared,
    notify: &Sender<Msg>,
    seen: &mut Sessions,
) -> zbus::Result<()> {
    let manager = Proxy::new(
        conn,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )?;
    let sessions: Vec<(String, u32, String, String, OwnedObjectPath)> =
        manager.call("ListSessions", &())?;
    let mut current = Sessions::new();
    for (_id, uid, _user, _seat, path) in sessions {
        if state.lock().unwrap().prompt_timeout(uid).is_none() {
            continue;
        }
        // A session that vanishes between the list and the property reads
        // is normal on logout, and logout emits the very signal that starts
        // this sweep. Skipping it must not skip the locked session after it.
        match sweep_session(conn, state, uid, &path) {
            Ok(Some(locked)) => {
                current.insert(path.as_str().to_string(), SessionLock { uid, locked });
            }
            Ok(None) => {}
            Err(e) => debug!(uid, "logind: session {path}: {e}"),
        }
    }
    apply_lock_changes(state, notify, seen, &current);
    *seen = current;
    Ok(())
}

fn sweep_session(
    conn: &Connection,
    state: &Shared,
    uid: u32,
    path: &OwnedObjectPath,
) -> zbus::Result<Option<bool>> {
    let session = Proxy::new(
        conn,
        "org.freedesktop.login1",
        path.as_str(),
        "org.freedesktop.login1.Session",
    )?;
    let class: String = session.get_property("Class")?;
    let kind: String = session.get_property("Type")?;
    let active: bool = session.get_property("Active")?;
    if class != "user" || !active || (kind != "wayland" && kind != "x11") {
        return Ok(None);
    }
    if let Ok(timestamp) = session.get_property::<u64>("Timestamp")
        && let Some(start) = start_instant(timestamp)
    {
        // With more than one graphical session the newest start is the one
        // the autostart window (DESIGN 5.5) should be measured from, and
        // ListSessions has no defined order.
        let mut st = state.lock().unwrap();
        let newest = st.session_start(uid).is_none_or(|prev| start > prev);
        if newest {
            st.set_session_start(uid, start);
        }
    }
    let is_locked: bool = session.get_property("LockedHint")?;
    Ok(Some(is_locked))
}

#[derive(Debug, PartialEq, Eq)]
enum LockChange {
    /// Every active graphical session of the user just became locked.
    Locked,
    /// An active graphical session just became unlocked.
    Unlocked,
    /// No change since the last look.
    None,
}

/// Whether every active graphical session of `uid` is locked. None when the
/// user has no active graphical session.
fn all_locked(sessions: &Sessions, uid: u32) -> Option<bool> {
    let mut any = false;
    let mut locked = true;
    for session in sessions.values().filter(|session| session.uid == uid) {
        any = true;
        locked &= session.locked;
    }
    any.then_some(locked)
}

/// Compare one user across two sweeps. A user counts as locked only when all
/// of their active graphical sessions are locked, so a locked session next to
/// an unlocked one does not clear tickets.
fn lock_change(previous: &Sessions, current: &Sessions, uid: u32) -> LockChange {
    match (all_locked(previous, uid), all_locked(current, uid)) {
        (_, None) => LockChange::None,
        (None, Some(true)) | (Some(false), Some(true)) => LockChange::Locked,
        (Some(true), Some(false)) => LockChange::Unlocked,
        _ => LockChange::None,
    }
}

fn apply_lock_changes(
    state: &Shared,
    notify: &Sender<Msg>,
    previous: &Sessions,
    current: &Sessions,
) {
    let mut uids: HashSet<u32> = previous.values().map(|session| session.uid).collect();
    uids.extend(current.values().map(|session| session.uid));
    for uid in uids {
        match lock_change(previous, current, uid) {
            LockChange::Locked => {
                let cleared = state.lock().unwrap().screen_locked(uid);
                for app in cleared {
                    debug!(uid, %app, "tickets cleared, every graphical session is locked");
                    let _ = notify.send(Msg::StateChanged { uid, app, unlocked: false });
                }
            }
            LockChange::Unlocked => {
                // KWin drops the window changes the script made while the
                // screen was locked, so the gate has to run again now that a
                // session is unlocked.
                debug!(uid, "a graphical session unlocked, the window layer has to reapply");
                let _ = notify.send(Msg::SessionUnlocked { uid });
            }
            LockChange::None => {}
        }
    }
}

/// Turn logind's microsecond wall-clock timestamp into a monotonic instant.
fn start_instant(timestamp_us: u64) -> Option<Instant> {
    let now_us = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_micros() as u64;
    let age = Duration::from_micros(now_us.saturating_sub(timestamp_us));
    Instant::now().checked_sub(age)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sessions(items: &[(u32, bool)]) -> Sessions {
        items
            .iter()
            .enumerate()
            .map(|(i, (uid, locked))| {
                (
                    format!("session-{i}"),
                    SessionLock {
                        uid: *uid,
                        locked: *locked,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn a_user_is_locked_only_when_every_active_session_is_locked() {
        assert_eq!(all_locked(&sessions(&[]), 1), None);
        assert_eq!(all_locked(&sessions(&[(1, true)]), 1), Some(true));
        assert_eq!(all_locked(&sessions(&[(1, false)]), 1), Some(false));
        assert_eq!(all_locked(&sessions(&[(1, true), (1, false)]), 1), Some(false));
        assert_eq!(all_locked(&sessions(&[(1, true), (1, true)]), 1), Some(true));
        assert_eq!(all_locked(&sessions(&[(2, true)]), 1), None);
    }

    #[test]
    fn lock_changes_follow_every_session_of_the_user() {
        let empty = sessions(&[]);
        let one_locked = sessions(&[(1, true)]);
        let one_unlocked = sessions(&[(1, false)]);
        let mixed = sessions(&[(1, true), (1, false)]);
        let all_locked = sessions(&[(1, true), (1, true)]);
        let other_user = sessions(&[(2, true)]);

        assert_eq!(lock_change(&empty, &one_locked, 1), LockChange::Locked);
        assert_eq!(lock_change(&one_locked, &one_locked, 1), LockChange::None);
        assert_eq!(lock_change(&one_locked, &one_unlocked, 1), LockChange::Unlocked);
        assert_eq!(lock_change(&one_unlocked, &one_locked, 1), LockChange::Locked);
        assert_eq!(lock_change(&one_locked, &mixed, 1), LockChange::Unlocked);
        assert_eq!(lock_change(&mixed, &all_locked, 1), LockChange::Locked);
        assert_eq!(lock_change(&all_locked, &mixed, 1), LockChange::Unlocked);
        assert_eq!(lock_change(&mixed, &one_locked, 1), LockChange::Locked);
        assert_eq!(lock_change(&one_locked, &empty, 1), LockChange::None);
        assert_eq!(lock_change(&empty, &one_unlocked, 1), LockChange::None);
        assert_eq!(lock_change(&one_locked, &other_user, 1), LockChange::None);
    }

    #[test]
    fn recent_timestamps_map_to_the_past() {
        let now_us = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros() as u64;
        let start = start_instant(now_us - 5_000_000).unwrap();
        let age = Instant::now().saturating_duration_since(start);
        assert!(age >= Duration::from_secs(4) && age < Duration::from_secs(7), "{age:?}");
    }

    #[test]
    fn a_timestamp_in_the_future_does_not_panic() {
        let now_us = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros() as u64;
        assert!(start_instant(now_us + 5_000_000).is_some());
    }
}
