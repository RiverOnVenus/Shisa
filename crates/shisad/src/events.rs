//! Completion notices from the authentication threads to the event loop.
//!
//! The loop only blocks on poll. A worker whose polkit check finishes pushes
//! an event and writes one byte to the pipe, which makes the loop wake up and
//! answer the request that was waiting.

use crate::policy::Change;
use std::io;
use std::os::fd::RawFd;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug)]
pub enum Event {
    /// The user authenticated for this request.
    Granted {
        uid: u32,
        app: String,
        request: u64,
    },
    /// The check did not answer before the wait limit. What waited for this
    /// request is refused at once (DESIGN 6.4).
    Cancelled { request: u64 },
    /// polkit answered that the user is not authorized. The box ended
    /// without a yes, and what waited is refused now.
    Denied { request: u64 },
    /// The authentication for this request ended without a result the user
    /// can retry, for example a polkit error. What waited is refused now
    /// instead of sitting until the wait limit.
    Failed { request: u64, why: &'static str },
    /// The system bus connection went away. The daemon cannot authenticate
    /// anything any more, so the main loop shuts down and systemd restarts it.
    BusLost,
    /// A D-Bus client asked for a settings change that needs authentication.
    ChangeRequest {
        id: u64,
        uid: u32,
        app: String,
        change: Change,
        /// Unique bus name of the caller, the polkit subject for the check.
        sender: String,
    },
    /// A D-Bus client asked to authenticate for an app and leave a ticket.
    UnlockRequest {
        id: u64,
        uid: u32,
        app: String,
        sender: String,
    },
}

pub struct Events {
    queue: Mutex<Vec<Event>>,
    next: AtomicU64,
    read: RawFd,
    write: RawFd,
}

impl Events {
    pub fn new() -> io::Result<Self> {
        let mut fds = [0; 2];
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Events {
            queue: Mutex::new(Vec::new()),
            next: AtomicU64::new(1),
            read: fds[0],
            write: fds[1],
        })
    }

    /// A fresh request number. Held execs and settings changes draw from the
    /// same counter, so a number names exactly one of them.
    pub fn next_id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::SeqCst)
    }

    pub fn fd(&self) -> RawFd {
        self.read
    }

    pub fn push(&self, event: Event) {
        self.queue.lock().unwrap().push(event);
        let byte = 1u8;
        // A full pipe still leaves the event queued, and the loop drains the
        // queue whenever it wakes up for any other reason.
        unsafe { libc::write(self.write, (&raw const byte).cast(), 1) };
    }

    pub fn drain(&self) -> Vec<Event> {
        let mut buf = [0u8; 64];
        loop {
            let n = unsafe { libc::read(self.read, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
        }
        std::mem::take(&mut *self.queue.lock().unwrap())
    }
}

impl Drop for Events {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.read);
            libc::close(self.write);
        }
    }
}
