//! User database lookups. shisad works with uids; the config files are named
//! after users, so both directions are needed.

use std::ffi::{CStr, CString, OsStr};
use std::mem;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::ptr;

const BUF_SIZE: usize = 16384;

fn lookup(name: Option<&str>, uid: Option<u32>) -> Option<(String, u32, PathBuf)> {
    let cname = match name {
        Some(n) => Some(CString::new(n).ok()?),
        None => None,
    };
    let mut pwd: libc::passwd = unsafe { mem::zeroed() };
    let mut buf = vec![0 as libc::c_char; BUF_SIZE];
    let mut result = ptr::null_mut();
    let status = unsafe {
        match (&cname, uid) {
            (Some(name), _) => libc::getpwnam_r(
                name.as_ptr(),
                &mut pwd,
                buf.as_mut_ptr(),
                buf.len(),
                &mut result,
            ),
            (None, Some(uid)) => libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result),
            (None, None) => return None,
        }
    };
    if status != 0 || result.is_null() {
        return None;
    }
    let name = unsafe { CStr::from_ptr(pwd.pw_name) }.to_string_lossy().into_owned();
    let home = unsafe { CStr::from_ptr(pwd.pw_dir) };
    Some((name, pwd.pw_uid, PathBuf::from(OsStr::from_bytes(home.to_bytes()))))
}

/// Account name of a uid.
pub fn name(uid: u32) -> Option<String> {
    lookup(None, Some(uid)).map(|(name, _, _)| name)
}

/// Home directory of a uid.
pub fn home(uid: u32) -> Option<PathBuf> {
    lookup(None, Some(uid)).map(|(_, _, home)| home)
}

/// uid of an account name.
pub fn uid(name: &str) -> Option<u32> {
    lookup(Some(name), None).map(|(_, uid, _)| uid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_account_round_trips() {
        let uid = unsafe { libc::getuid() };
        let name = name(uid).expect("own user");
        assert_eq!(uid_of_name(&name), Some(uid));
        assert!(home(uid).is_some_and(|h| h.is_absolute()));
    }

    fn uid_of_name(name: &str) -> Option<u32> {
        uid(name)
    }

    #[test]
    fn unknown_users() {
        assert_eq!(name(4_000_000_000), None);
        assert_eq!(uid("no-such-user-here"), None);
    }
}
