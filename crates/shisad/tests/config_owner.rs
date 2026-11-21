//! The real owner check, exercised in a user namespace mapped to root.
//!
//! The daemon requires each config directory and file to be root's and not
//! group- or world-writable (docs/subsystems/configuration.md). A unit test
//! running as the normal user cannot create such files, so each test here
//! re-executes itself under `unshare --user --map-root-user`. Inside that
//! namespace the test is uid 0 and the files it creates are root's, which is
//! the same pair the daemon sees. When user namespaces are unavailable the
//! test reports a skip instead of silently passing as a different check.

use shisad::load::{self, Marker, Marks};
use shisad::policy::{AppPolicy, Policy, UserPolicy};
use shisad::procfs::FileId;
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Set in the re-executed child. The parent never runs a test body.
const CHILD: &str = "SHISA_USERNS_TEST";
/// The account the config file belongs to, resolved against the host passwd.
/// The daemon maps a file to a uid the same way.
const USER: &str = "SHISA_USERNS_USER";

struct FakeMarker;

impl Marker for FakeMarker {
    fn mark(&self, _path: &Path) -> std::io::Result<FileId> {
        Ok(FileId { dev: 1, ino: 10 })
    }

    fn unmark(&self, _path: &Path, _id: FileId) -> std::io::Result<bool> {
        Ok(true)
    }
}

/// True when the calling process should run the test body. Otherwise the
/// parent has started a child inside a root-mapped user namespace and this
/// function either saw the child finish or reported a skip.
fn enter_namespace(test: &str) -> bool {
    if std::env::var_os(CHILD).is_some() {
        return true;
    }
    let user = shisa_core::users::name(unsafe { libc::getuid() }).expect("own account");
    let probe = Command::new("unshare")
        .args(["--user", "--map-root-user", "true"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if !probe.is_ok_and(|status| status.success()) {
        eprintln!("skip {test}: user namespaces are unavailable");
        return false;
    }
    let exe = std::env::current_exe().expect("test binary");
    let status = Command::new("unshare")
        .args(["--user", "--map-root-user"])
        .arg(exe)
        .args(["--exact", test, "--test-threads=1", "--nocapture"])
        .env(CHILD, "1")
        .env(USER, user)
        .status()
        .expect("run the test under unshare");
    assert!(status.success(), "{test} failed inside the user namespace");
    false
}

fn config_dir(tag: &str) -> (PathBuf, String) {
    let user = std::env::var(USER).expect("set by the parent");
    let dir = std::env::temp_dir().join(format!("shisa-userns-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    (dir, user)
}

fn write_config(dir: &Path, user: &str, mode: u32) -> PathBuf {
    let bin = dir.join("bin");
    fs::write(&bin, "x").unwrap();
    let path = dir.join(format!("{user}.toml"));
    fs::write(
        &path,
        format!("[[apps]]\nid = \"app\"\npaths = [\"{}\"]\n", bin.display()),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    path
}

/// A policy for the test account with one app that is easy to tell apart
/// from a freshly loaded one.
fn previous_policy(user: &str) -> (Policy, u32) {
    let uid = shisa_core::users::uid(user).expect("host account");
    let app = AppPolicy {
        id: "previous".into(),
        grace: Duration::from_secs(300),
        autostart: false,
        relock: Duration::ZERO,
        paths: vec!["/usr/bin/previous".into()],
        effective: vec!["/usr/bin/previous".into()],
        files: vec![FileId { dev: 1, ino: 9 }],
    };
    let policy = Policy {
        users: HashMap::from([(
            uid,
            UserPolicy {
                name: user.into(),
                prompt_timeout: Duration::from_secs(60),
                global_grace: Duration::from_secs(300),
                clear_on_lock: true,
                apps: vec![app],
            },
        )]),
    };
    (policy, uid)
}

#[test]
fn a_root_owned_config_is_loaded() {
    if !enter_namespace("a_root_owned_config_is_loaded") {
        return;
    }
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "the wrapper did not map root"
    );
    let (dir, user) = config_dir("accepted");
    write_config(&dir, &user, 0o644);
    let policy = load::load(&FakeMarker, &dir, &Policy::default(), &mut Marks::new());
    let uid = shisa_core::users::uid(&user).unwrap();
    let loaded = policy
        .users
        .get(&uid)
        .expect("the root-owned config must load");
    assert_eq!(loaded.apps[0].id, "app");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_group_writable_config_keeps_the_previous_policy() {
    if !enter_namespace("a_group_writable_config_keeps_the_previous_policy") {
        return;
    }
    let (dir, user) = config_dir("file-mode");
    write_config(&dir, &user, 0o664);
    let (old, uid) = previous_policy(&user);
    let policy = load::load(&FakeMarker, &dir, &old, &mut Marks::new());
    let kept = policy
        .users
        .get(&uid)
        .expect("the previous policy must stay");
    assert_eq!(kept.apps[0].id, "previous");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_group_writable_directory_keeps_the_previous_policy() {
    if !enter_namespace("a_group_writable_directory_keeps_the_previous_policy") {
        return;
    }
    let (dir, user) = config_dir("dir-mode");
    write_config(&dir, &user, 0o644);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o775)).unwrap();
    let (old, uid) = previous_policy(&user);
    let policy = load::load(&FakeMarker, &dir, &old, &mut Marks::new());
    let kept = policy
        .users
        .get(&uid)
        .expect("the previous policy must stay");
    assert_eq!(kept.apps[0].id, "previous");
    let _ = fs::remove_dir_all(&dir);
}
