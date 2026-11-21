//! fanotify exec-permission gate (docs/subsystems/launch-gate.md).
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
        response: if allow {
            libc::FAN_ALLOW
        } else {
            libc::FAN_DENY
        },
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
    Ok((
        FileId {
            dev: st.st_dev as u64,
            ino: st.st_ino as u64,
        },
        st.st_mode,
    ))
}

/// An O_PATH descriptor, closed on drop.
struct PathFd(RawFd);

impl PathFd {
    fn open(path: &Path) -> io::Result<Self> {
        let c = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(PathFd(fd))
        }
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
        Some(why) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            why.message(),
        )),
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
        if fan < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(Gate { fan })
        }
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
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
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
            libc::fanotify_mark(
                self.fan,
                flags,
                libc::FAN_OPEN_EXEC_PERM,
                libc::AT_FDCWD,
                link.as_ptr(),
            )
        };
        if r < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Read the events that are queued right now and hand each permission
    /// request to `handle`. Never blocks. An error means the event stream can
    /// no longer be trusted and the daemon should exit, which makes the kernel
    /// allow everything that is pending.
    pub fn drain(&self, mut handle: impl FnMut(Request)) -> io::Result<()> {
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
            while off < n {
                let (len, parsed) = parse_event(&buf[off..n])?;
                match parsed {
                    Parsed::Exec { fd, pid } => handle(Request {
                        fan: self.fan,
                        fd,
                        pid,
                        answered: false,
                    }),
                    Parsed::Skip(fd) => unsafe {
                        libc::close(fd);
                    },
                    Parsed::Overflow => {
                        error!("fanotify queue overflow, some exec requests were not seen")
                    }
                    Parsed::Ignore => {}
                }
                off += len;
            }
        }
    }
}

/// What one decoded fanotify record asks for.
#[derive(Debug)]
enum Parsed {
    /// A permission request for opening an executable.
    Exec { fd: RawFd, pid: i32 },
    /// A descriptor to close without handling it.
    Skip(RawFd),
    /// The kernel dropped events it could not queue. The stream stays valid.
    Overflow,
    /// A record with no descriptor that is not an overflow.
    Ignore,
}

fn bad_event() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "bad fanotify event")
}

/// Decode the first record in `buf` and say how many bytes it used. A record
/// that does not fit its buffer, or an unknown version, means the stream can
/// no longer be trusted. A dropped permission event is allowed by the kernel
/// without a decision, so overflow must stay visible rather than be skipped.
fn parse_event(buf: &[u8]) -> io::Result<(usize, Parsed)> {
    const META: usize = mem::size_of::<libc::fanotify_event_metadata>();
    if buf.len() < META {
        return Err(bad_event());
    }
    let ev: libc::fanotify_event_metadata =
        unsafe { std::ptr::read_unaligned(buf.as_ptr().cast()) };
    let len = ev.event_len as usize;
    if ev.vers != libc::FANOTIFY_METADATA_VERSION || len < META || len > buf.len() {
        return Err(bad_event());
    }
    if ev.fd < 0 {
        return Ok((
            len,
            if ev.mask & libc::FAN_Q_OVERFLOW != 0 {
                Parsed::Overflow
            } else {
                Parsed::Ignore
            },
        ));
    }
    if ev.mask & libc::FAN_OPEN_EXEC_PERM == 0 {
        return Ok((len, Parsed::Skip(ev.fd)));
    }
    Ok((
        len,
        Parsed::Exec {
            fd: ev.fd,
            pid: ev.pid,
        },
    ))
}

/// Decode one record for the fuzz target. Behind a feature so a normal build
/// does not publish a function no daemon code calls.
#[cfg(feature = "fuzz")]
pub fn fuzz_parse_event(buf: &[u8]) -> bool {
    parse_event(buf).is_ok()
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

    const META: usize = std::mem::size_of::<libc::fanotify_event_metadata>();

    fn event(event_len: u32, mask: u64, fd: i32, pid: i32) -> libc::fanotify_event_metadata {
        libc::fanotify_event_metadata {
            event_len,
            vers: libc::FANOTIFY_METADATA_VERSION,
            reserved: 0,
            metadata_len: META as u16,
            mask,
            fd,
            pid,
        }
    }

    fn record(ev: &libc::fanotify_event_metadata) -> Vec<u8> {
        let mut buf = vec![0u8; ev.event_len as usize];
        unsafe { std::ptr::write_unaligned(buf.as_mut_ptr().cast(), *ev) };
        buf
    }

    #[test]
    fn an_exec_permission_record_names_its_descriptor_and_pid() {
        let first = record(&event(META as u32, libc::FAN_OPEN_EXEC_PERM, 7, 4242));
        let second = record(&event(META as u32, libc::FAN_OPEN_EXEC_PERM, 8, 4243));
        let mut stream = first;
        stream.extend(second);

        let (len, parsed) = parse_event(&stream).unwrap();
        assert_eq!(len, META);
        assert!(matches!(parsed, Parsed::Exec { fd: 7, pid: 4242 }));
        // The returned length is where the next record starts.
        let (_, parsed) = parse_event(&stream[len..]).unwrap();
        assert!(matches!(parsed, Parsed::Exec { fd: 8, pid: 4243 }));
    }

    #[test]
    fn a_record_for_another_event_is_skipped() {
        let buf = record(&event(META as u32, libc::FAN_ACCESS, 7, 1));
        let (len, parsed) = parse_event(&buf).unwrap();
        assert_eq!(len, META);
        assert!(matches!(parsed, Parsed::Skip(7)));
    }

    #[test]
    fn an_overflow_is_reported_and_a_stray_negative_descriptor_is_ignored() {
        let overflow = record(&event(META as u32, libc::FAN_Q_OVERFLOW, -1, 0));
        let (_, parsed) = parse_event(&overflow).unwrap();
        assert!(matches!(parsed, Parsed::Overflow));

        let stray = record(&event(META as u32, libc::FAN_ACCESS, -1, 0));
        let (_, parsed) = parse_event(&stray).unwrap();
        assert!(matches!(parsed, Parsed::Ignore));
    }

    #[test]
    fn a_bad_version_is_refused() {
        let mut ev = event(META as u32, libc::FAN_OPEN_EXEC_PERM, 7, 1);
        ev.vers = 0;
        assert_eq!(
            parse_event(&record(&ev)).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn a_record_shorter_or_longer_than_its_header_is_refused() {
        let short = event((META - 1) as u32, libc::FAN_OPEN_EXEC_PERM, 7, 1);
        let mut buf = vec![0u8; META];
        unsafe { std::ptr::write_unaligned(buf.as_mut_ptr().cast(), short) };
        assert!(parse_event(&buf).is_err());

        let long = event((META + 8) as u32, libc::FAN_OPEN_EXEC_PERM, 7, 1);
        let mut buf = vec![0u8; META];
        unsafe { std::ptr::write_unaligned(buf.as_mut_ptr().cast(), long) };
        assert!(parse_event(&buf).is_err());
    }

    #[test]
    fn a_buffer_shorter_than_the_header_is_refused() {
        assert!(parse_event(&[0u8; 3]).is_err());
    }
}
