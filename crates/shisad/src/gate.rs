//! fanotify exec-permission gate (DESIGN 5.1).
//!
//! Only inode marks on regular files are ever placed. Mount and filesystem
//! marks are not used anywhere, so execs of unprotected programs never reach
//! this process. Every permission event becomes a [`Request`], and a request
//! that is dropped without an answer denies, so no event is left pending.

use crate::procfs::FileId;
use std::ffi::CString;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::{fs, io, mem};
use tracing::error;

pub struct Gate {
    fan: RawFd,
}

/// One pending exec. The caller is stopped inside execve until it is answered.
pub struct Request {
    fan: RawFd,
    fd: RawFd,
    pub pid: i32,
    answered: bool,
}

impl Request {
    /// The file being executed.
    pub fn file(&self) -> io::Result<FileId> {
        fstat_id(self.fd).map(|(id, _)| id)
    }

    pub fn answer(mut self, allow: bool) -> io::Result<()> {
        self.answered = true;
        respond(self.fan, self.fd, allow)
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        if !self.answered
            && let Err(e) = respond(self.fan, self.fd, false)
        {
            error!("unanswered request for pid {}: deny failed: {e}", self.pid);
        }
        unsafe { libc::close(self.fd) };
    }
}

fn respond(fan: RawFd, fd: RawFd, allow: bool) -> io::Result<()> {
    let resp = libc::fanotify_response {
        fd,
        response: if allow { libc::FAN_ALLOW } else { libc::FAN_DENY },
    };
    loop {
        let n = unsafe { libc::write(fan, (&raw const resp).cast(), mem::size_of_val(&resp)) };
        if n >= 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

fn fstat_id(fd: RawFd) -> io::Result<(FileId, libc::mode_t)> {
    let mut st: libc::stat = unsafe { mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((FileId { dev: st.st_dev as u64, ino: st.st_ino as u64 }, st.st_mode))
}

/// An O_PATH descriptor, closed on drop.
struct PathFd(RawFd);

impl PathFd {
    fn open(path: &Path) -> io::Result<Self> {
        let c = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        if fd < 0 { Err(io::Error::last_os_error()) } else { Ok(PathFd(fd)) }
    }
}

impl Drop for PathFd {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

/// Reject a descriptor whose opened object is a shell, interpreter or one of
/// Shisa's own programs. The check runs on the pinned descriptor, so a path
/// swapped between the earlier validation and the mark cannot redirect the
/// mark to a different inode.
fn ensure_suitable(fd: RawFd) -> io::Result<()> {
    let real = fs::read_link(format!("/proc/self/fd/{fd}"))?;
    match shisa_core::desktop::unsuitable(&real) {
        Some(why) => Err(io::Error::new(io::ErrorKind::PermissionDenied, why)),
        None => Ok(()),
    }
}

impl Gate {
    pub fn new() -> io::Result<Self> {
        let fan = unsafe {
            libc::fanotify_init(
                libc::FAN_CLOEXEC | libc::FAN_NONBLOCK | libc::FAN_CLASS_CONTENT,
                (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_LARGEFILE) as u32,
            )
        };
        if fan < 0 { Err(io::Error::last_os_error()) } else { Ok(Gate { fan }) }
    }

    pub fn fd(&self) -> RawFd {
        self.fan
    }

    /// Mark the inode `path` resolves to. Symlinks are followed. The file is
    /// pinned by a descriptor between the stat and the mark, and the safety
    /// check runs on that descriptor, so the checked and marked inode is the
    /// same one.
    pub fn mark(&self, path: &Path) -> io::Result<FileId> {
        let fd = PathFd::open(path)?;
        let (id, mode) = fstat_id(fd.0)?;
        if mode & libc::S_IFMT != libc::S_IFREG {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"));
        }
        ensure_suitable(fd.0)?;
        self.change(libc::FAN_MARK_ADD, &fd)?;
        Ok(id)
    }

    /// Remove the mark from `id` if `path` still leads to that inode. When the
    /// file was replaced the old inode cannot be reached by path any more. Its
    /// mark then stays until the inode goes away, and events from it are
    /// allowed because no policy refers to it.
    pub fn unmark(&self, path: &Path, id: FileId) -> io::Result<bool> {
        let fd = PathFd::open(path)?;
        if fstat_id(fd.0)?.0 != id {
            return Ok(false);
        }
        self.change(libc::FAN_MARK_REMOVE, &fd)?;
        Ok(true)
    }

    fn change(&self, flags: libc::c_uint, fd: &PathFd) -> io::Result<()> {
        // No FAN_MARK_MOUNT or FAN_MARK_FILESYSTEM: this is an inode mark.
        let link = CString::new(format!("/proc/self/fd/{}", fd.0)).unwrap();
        let r = unsafe {
            libc::fanotify_mark(self.fan, flags, libc::FAN_OPEN_EXEC_PERM, libc::AT_FDCWD, link.as_ptr())
        };
        if r < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
    }

    /// Read the events that are queued right now and hand each permission
    /// request to `handle`. Never blocks. An error means the event stream can
    /// no longer be trusted and the daemon should exit, which makes the kernel
    /// allow everything that is pending.
    pub fn drain(&self, mut handle: impl FnMut(Request)) -> io::Result<()> {
        const META: usize = mem::size_of::<libc::fanotify_event_metadata>();
        let mut buf = [0u8; 4096];
        loop {
            let n = unsafe { libc::read(self.fan, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                let e = io::Error::last_os_error();
                match e.kind() {
                    io::ErrorKind::WouldBlock => return Ok(()),
                    io::ErrorKind::Interrupted => continue,
                    _ => return Err(e),
                }
            }
            let n = n as usize;
            if n == 0 {
                return Ok(());
            }
            let mut off = 0;
            while off + META <= n {
                let ev: libc::fanotify_event_metadata =
                    unsafe { std::ptr::read_unaligned(buf.as_ptr().add(off).cast()) };
                let len = ev.event_len as usize;
                if ev.vers != libc::FANOTIFY_METADATA_VERSION || len < META || off + len > n {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "bad fanotify event"));
                }
                off += len;
                if ev.fd < 0 {
                    // The kernel drops events it cannot queue, and a dropped
                    // permission request is never answered, so that exec was
                    // allowed without a decision. Make the gap visible.
                    if ev.mask & libc::FAN_Q_OVERFLOW != 0 {
                        error!("fanotify queue overflow, some exec requests were not seen");
                    }
                    continue;
                }
                if ev.mask & libc::FAN_OPEN_EXEC_PERM == 0 {
                    unsafe { libc::close(ev.fd) };
                    continue;
                }
                handle(Request { fan: self.fan, fd: ev.fd, pid: ev.pid, answered: false });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn an_opened_shell_is_refused() {
        let dir = std::env::temp_dir().join(format!("shisa-gate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let plain = dir.join("plain");
        fs::write(&plain, b"#!/bin/sh\n").unwrap();
        let ok = PathFd::open(&plain).unwrap();
        assert!(ensure_suitable(ok.0).is_ok());

        let link = dir.join("link");
        symlink("/usr/bin/bash", &link).unwrap();
        let bad = PathFd::open(&link).unwrap();
        assert_eq!(
            ensure_suitable(bad.0).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
