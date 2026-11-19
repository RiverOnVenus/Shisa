//! sd_notify without libsystemd: READY and the watchdog keep-alive
//! (DESIGN 5.3). Without $NOTIFY_SOCKET every call is a no-op.

use std::env;
use std::mem;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;

pub struct Notify {
    sock: RawFd,
    addr: libc::sockaddr_un,
    len: libc::socklen_t,
}

/// Bytes available in `sun_path`. A filesystem name needs its trailing NUL
/// there too, an abstract name its leading NUL.
fn name_fits(len: usize) -> bool {
    let room = mem::size_of::<libc::sockaddr_un>() - mem::offset_of!(libc::sockaddr_un, sun_path);
    len < room
}

impl Notify {
    pub fn connect() -> Option<Self> {
        let path = env::var_os("NOTIFY_SOCKET")?;
        let bytes = path.as_bytes();
        // A leading '@' names an abstract socket. Its name starts at
        // sun_path[1], behind the NUL that marks the address as abstract. A
        // filesystem path starts at sun_path[0] and ends with its own NUL.
        let abstract_socket = bytes.first() == Some(&b'@');
        let name = if abstract_socket { &bytes[1..] } else { bytes };
        if name.is_empty() || !name_fits(name.len()) {
            return None;
        }
        let mut addr: libc::sockaddr_un = unsafe { mem::zeroed() };
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let start = usize::from(abstract_socket);
        for (dst, src) in addr.sun_path[start..].iter_mut().zip(name) {
            *dst = *src as libc::c_char;
        }
        let len = if abstract_socket {
            mem::size_of::<libc::sa_family_t>() + 1 + name.len()
        } else {
            mem::size_of::<libc::sa_family_t>() + name.len() + 1
        } as libc::socklen_t;
        // Non-blocking: a full notify socket must never stall the fanotify
        // loop. A dropped WATCHDOG=1 only matters if it happens for a whole
        // interval, and READY is retried by nothing, but neither is worth a
        // blocked main thread.
        let sock = unsafe {
            libc::socket(
                libc::AF_UNIX,
                libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                0,
            )
        };
        if sock < 0 {
            return None;
        }
        Some(Notify { sock, addr, len })
    }

    fn send(&self, message: &str) -> bool {
        // A descriptor longer than the struct is what makes sendto return
        // EINVAL, and with Type=notify systemd would then never hear READY.
        debug_assert!(self.len as usize <= mem::size_of::<libc::sockaddr_un>());
        let bytes = message.as_bytes();
        let sent = unsafe {
            libc::sendto(
                self.sock,
                bytes.as_ptr().cast(),
                bytes.len(),
                libc::MSG_NOSIGNAL,
                (&raw const self.addr).cast(),
                self.len,
            )
        };
        sent == bytes.len() as isize
    }

    pub fn ready(&self) -> bool {
        self.send("READY=1\nSTATUS=waiting for exec requests\n")
    }

    pub fn watchdog(&self) -> bool {
        self.send("WATCHDOG=1\n")
    }

    pub fn stopping(&self) -> bool {
        self.send("STOPPING=1\n")
    }
}

impl Drop for Notify {
    fn drop(&mut self) {
        unsafe { libc::close(self.sock) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The abstract form is what a container manager may pass. The kernel
    /// reads the name from sun_path[1], so a name written at [0] goes to a
    /// filesystem socket that does not exist and the message is lost.
    #[test]
    fn an_abstract_notify_socket_gets_the_datagram() {
        let name = format!("shisa-notify-{}", std::process::id());
        let sock = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
        assert!(sock >= 0);
        unsafe {
            let mut addr: libc::sockaddr_un = mem::zeroed();
            addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
            for (dst, src) in addr.sun_path[1..].iter_mut().zip(name.as_bytes()) {
                *dst = *src as libc::c_char;
            }
            let len = mem::size_of::<libc::sa_family_t>() + 1 + name.len();
            assert_eq!(
                libc::bind(sock, (&raw const addr).cast(), len as libc::socklen_t),
                0
            );
            // No other test reads this variable.
            std::env::set_var("NOTIFY_SOCKET", format!("@{name}"));
            let notify = Notify::connect().expect("abstract notify socket");
            assert!(notify.send("PING=1\n"));
            let mut buf = [0u8; 16];
            let n = libc::recv(sock, buf.as_mut_ptr().cast(), buf.len(), 0);
            assert_eq!(&buf[..n.max(0) as usize], b"PING=1\n");
            std::env::remove_var("NOTIFY_SOCKET");
            libc::close(sock);
        }
    }
}
