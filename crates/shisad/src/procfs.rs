//! What shisad needs to know about a calling process, read from /proc.

use std::fs;
use std::os::unix::fs::MetadataExt;

/// Identity of a file, independent of the path it was reached by. Paths in
/// events can be sandbox-internal (docs/subsystems/launch-gate.md), so apps are matched by inode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId {
    pub dev: u64,
    pub ino: u64,
}

/// A process, told apart from a later reuse of its pid by its start time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcId {
    pub pid: i32,
    pub start: u64,
}

pub trait Procs {
    /// Real uid of the process.
    fn uid(&self, pid: i32) -> Option<u32>;
    /// The image the process currently runs.
    fn exe(&self, pid: i32) -> Option<FileId>;
    /// Parent pid and start time.
    fn stat(&self, pid: i32) -> Option<(i32, u64)>;
    /// cgroup v2 path of the process.
    fn cgroup(&self, pid: i32) -> Option<String>;
}

pub struct RealProcs;

impl Procs for RealProcs {
    fn uid(&self, pid: i32) -> Option<u32> {
        parse_uid(&fs::read_to_string(format!("/proc/{pid}/status")).ok()?)
    }

    fn exe(&self, pid: i32) -> Option<FileId> {
        let m = fs::metadata(format!("/proc/{pid}/exe")).ok()?;
        Some(FileId { dev: m.dev(), ino: m.ino() })
    }

    fn stat(&self, pid: i32) -> Option<(i32, u64)> {
        parse_stat(&fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
    }

    fn cgroup(&self, pid: i32) -> Option<String> {
        parse_cgroup(&fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?)
    }
}

fn parse_uid(status: &str) -> Option<u32> {
    let line = status.lines().find(|l| l.starts_with("Uid:"))?;
    line.split_ascii_whitespace().nth(1)?.parse().ok()
}

/// Path of the unified hierarchy line, `0::/user.slice/...`.
fn parse_cgroup(cgroup: &str) -> Option<String> {
    cgroup.lines().find_map(|l| l.strip_prefix("0::").map(str::to_string))
}

/// Parent pid (field 4) and start time (field 22) of /proc/<pid>/stat. The
/// command name in field 2 may itself contain spaces and parentheses, so
/// parsing starts after the last closing parenthesis.
fn parse_stat(stat: &str) -> Option<(i32, u64)> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let mut fields = rest.split_ascii_whitespace();
    let ppid = fields.nth(1)?.parse().ok()?;
    let start = fields.nth(17)?.parse().ok()?;
    Some((ppid, start))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_with_awkward_comm() {
        let line = "4242 (a b) c)) S 77 4242 4242 0 -1 4194304 100 0 0 0 1 2 0 0 20 0 1 0 987654 1000 200 18446744073709551615 0 0 0";
        assert_eq!(parse_stat(line), Some((77, 987654)));
        assert_eq!(parse_stat("garbage"), None);
        assert_eq!(parse_stat("1 (x) S 0"), None);
    }

    #[test]
    fn uid_line() {
        assert_eq!(parse_uid("Name:\tx\nUid:\t1000\t1001\t1002\t1003\nGid:\t5\n"), Some(1000));
        assert_eq!(parse_uid("Name:\tx\n"), None);
    }

    #[test]
    fn cgroup_line() {
        assert_eq!(parse_cgroup("0::/user.slice/x.service\n"), Some("/user.slice/x.service".into()));
        assert_eq!(parse_cgroup("1:systemd:/legacy\n"), None);
    }

    #[test]
    fn reads_own_process() {
        let me = std::process::id() as i32;
        assert_eq!(RealProcs.uid(me), Some(unsafe { libc::getuid() }));
        let (ppid, start) = RealProcs.stat(me).unwrap();
        assert_eq!(ppid, unsafe { libc::getppid() });
        assert!(start > 0);
        let exe = fs::metadata(std::env::current_exe().unwrap()).unwrap();
        assert_eq!(RealProcs.exe(me), Some(FileId { dev: exe.dev(), ino: exe.ino() }));
        assert!(RealProcs.cgroup(me).is_some_and(|c| c.starts_with('/')));
        assert_eq!(RealProcs.uid(-1), None);
    }
}
