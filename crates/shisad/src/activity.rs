//! The recent launch decisions the settings window shows.
//!
//! The event loop only records here: one short lock over a small deque, no
//! I/O, so a launch never waits on the window. Every entry belongs to one
//! user and `recent` returns only the caller's own.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Decisions kept per user for the window. Older ones fall off the front.
const KEEP: usize = 64;

/// The kinds a launch can end with. `ASKED` is the only one that changes
/// later: the event loop resolves it to `GRANTED` or one of the failures.
pub const ASKED: &str = "asked";
pub const GRANTED: &str = "granted";
/// polkit answered that the user is not authorized.
pub const DENIED: &str = "denied";
/// The wait limit passed before the check answered. `TIMEOUT` is the event
/// loop's own sweep, `CANCELLED` a check that returned after the limit.
pub const TIMEOUT: &str = "timeout";
pub const CANCELLED: &str = "cancelled";
/// The check itself could not run or broke off.
pub const FAILED: &str = "failed";
/// A lock revoked the waiting request before its answer could commit.
pub const REVOKED: &str = "revoked";
pub const TICKET: &str = "ticket";
pub const INSTANCE: &str = "instance";
pub const AUTOSTART: &str = "autostart";
pub const SELF_EXEC: &str = "selfexec";

/// A launch decision as the bus hands it out. The shape lives in shisa-core
/// because the settings window deserializes the same tuple.
pub type Row = shisa_core::dbus::LaunchRow;

struct Launch {
    id: u64,
    at: i64,
    app: String,
    pid: i32,
    kind: &'static str,
    until: i64,
    resolved_at: i64,
}

#[derive(Default, Clone, Copy)]
struct Counts {
    requests: u64,
    allowed: u64,
    denied: u64,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<u32, VecDeque<Launch>>,
    counts: HashMap<u32, Counts>,
    // A pending group can outlive its display row. Keep its owner until the
    // first answer so the lifetime totals survive eviction and duplicate replies.
    pending: HashMap<u64, u32>,
}

#[derive(Default)]
pub struct Log {
    inner: Mutex<Inner>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A deadline `d` from now, in the unix milliseconds the row carries.
pub fn after(d: Duration) -> i64 {
    now_ms() + d.as_millis() as i64
}

impl Log {
    /// A launch that is already decided, or one that starts waiting for the
    /// user. `until` is `after(timeout)` for `ASKED`, zero otherwise.
    pub fn record(&self, uid: u32, id: u64, app: &str, pid: i32, kind: &'static str, until: i64) {
        let mut inner = self.inner.lock().unwrap();
        let counts = inner.counts.entry(uid).or_default();
        counts.requests += 1;
        match kind {
            ASKED => {
                inner.pending.insert(id, uid);
            }
            GRANTED | TICKET | INSTANCE | AUTOSTART | SELF_EXEC => counts.allowed += 1,
            _ => counts.denied += 1,
        }
        let entries = inner.entries.entry(uid).or_default();
        if entries.len() == KEEP {
            entries.pop_front();
        }
        entries.push_back(Launch {
            id,
            at: now_ms(),
            app: app.to_string(),
            pid,
            kind,
            until,
            resolved_at: if kind == ASKED { 0 } else { now_ms() },
        });
    }

    /// The box answered or gave up: set the final kind on the record that
    /// waited under this id. Unknown ids are ignored, as a late answer to a
    /// request the loop already dropped must be.
    pub fn resolve(&self, id: u64, kind: &'static str) {
        let mut inner = self.inner.lock().unwrap();
        let Some(uid) = inner.pending.remove(&id) else {
            return;
        };
        if let Some(entry) = inner
            .entries
            .get_mut(&uid)
            .and_then(|entries| entries.iter_mut().find(|e| e.id == id))
        {
            entry.kind = kind;
            entry.until = 0;
            entry.resolved_at = now_ms();
        }
        if let Some(counts) = inner.counts.get_mut(&uid) {
            match kind {
                GRANTED => counts.allowed += 1,
                _ => counts.denied += 1,
            }
        }
    }

    /// This user's launches, oldest first, and the totals since the daemon
    /// started: requests, allowed, denied.
    pub fn recent(&self, uid: u32) -> (Vec<Row>, u64, u64, u64) {
        let inner = self.inner.lock().unwrap();
        let rows = inner
            .entries
            .get(&uid)
            .into_iter()
            .flatten()
            .map(|e| {
                (
                    e.id,
                    e.at,
                    e.app.clone(),
                    e.pid,
                    e.kind.to_string(),
                    e.until,
                    e.resolved_at,
                )
            })
            .collect();
        let counts = inner.counts.get(&uid).copied().unwrap_or_default();
        (rows, counts.requests, counts.allowed, counts.denied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_the_last_entries() {
        let log = Log::default();
        for i in 0..(KEEP as u64 + 10) {
            log.record(1000, i, "app", i as i32, TICKET, 0);
        }
        let (rows, requests, allowed, denied) = log.recent(1000);
        assert_eq!(rows.len(), KEEP);
        assert_eq!(rows.first().unwrap().0, 10);
        assert_eq!(requests, KEEP as u64 + 10);
        assert_eq!(allowed, KEEP as u64 + 10);
        assert_eq!(denied, 0);
    }

    #[test]
    fn each_user_keeps_its_own_rows() {
        let log = Log::default();
        for i in 0..(KEEP as u64 + 5) {
            log.record(1000, i, "mine", i as i32, TICKET, 0);
        }
        for i in 0..3 {
            log.record(2000, i, "theirs", i as i32, TICKET, 0);
        }
        assert_eq!(log.recent(1000).0.len(), KEEP);
        assert_eq!(log.recent(2000).0.len(), 3);
        // The other account's rows did not evict this one's.
        assert_eq!(log.recent(1000).0.first().unwrap().0, 5);
    }

    #[test]
    fn resolve_moves_a_waiting_row_and_counts_it_once() {
        let log = Log::default();
        log.record(1000, 1, "app", 42, ASKED, after(Duration::from_secs(60)));
        let (rows, requests, allowed, denied) = log.recent(1000);
        assert_eq!(rows[0].4, ASKED);
        assert_eq!((requests, allowed, denied), (1, 0, 0));

        log.resolve(1, GRANTED);
        let (rows, requests, allowed, denied) = log.recent(1000);
        assert_eq!(rows[0].4, GRANTED);
        assert_eq!(rows[0].5, 0);
        assert!(rows[0].6 > 0);
        assert_eq!((requests, allowed, denied), (1, 1, 0));

        // A second answer for the same request is not counted again.
        log.resolve(1, DENIED);
        assert_eq!(log.recent(1000).2, 1);

        log.record(1000, 2, "app", 43, ASKED, 0);
        log.resolve(2, TIMEOUT);
        assert_eq!(log.recent(1000).3, 1);
        log.record(1000, 3, "app", 44, ASKED, 0);
        log.resolve(3, CANCELLED);
        log.record(1000, 4, "app", 45, ASKED, 0);
        log.resolve(4, FAILED);
        assert_eq!(log.recent(1000).3, 3);
    }

    #[test]
    fn only_the_callers_rows_and_totals() {
        let log = Log::default();
        log.record(1000, 1, "mine", 1, TICKET, 0);
        log.record(2000, 2, "theirs", 2, DENIED, 0);
        let (rows, requests, allowed, denied) = log.recent(1000);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].2, "mine");
        assert_eq!((requests, allowed, denied), (1, 1, 0));
        assert_eq!(log.recent(2000).1, 1);
        assert_eq!(log.recent(3000).0.len(), 0);
    }

    #[test]
    fn an_answer_after_row_eviction_still_counts_once() {
        let log = Log::default();
        log.record(1000, 1, "waiting", 42, ASKED, 0);
        for id in 2..=KEEP as u64 + 1 {
            log.record(1000, id, "busy", 43, TICKET, 0);
            log.record(2000, id + KEEP as u64, "other", 44, TICKET, 0);
        }
        log.resolve(1, DENIED);
        log.resolve(1, GRANTED);
        let (rows, requests, allowed, denied) = log.recent(1000);
        assert_eq!(rows.len(), KEEP);
        assert_eq!(rows[0].0, 2);
        assert_eq!(log.recent(2000).0.len(), KEEP);
        assert_eq!(log.recent(2000).3, 0);
        assert_eq!(
            (requests, allowed, denied),
            (KEEP as u64 + 1, KEEP as u64, 1)
        );
    }

    #[test]
    fn unknown_resolve_is_ignored() {
        let log = Log::default();
        log.resolve(9, GRANTED);
        assert_eq!(log.recent(1000).1, 0);
    }
}
