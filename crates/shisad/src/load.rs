//! Turn the per-user config files into a [`Policy`] and inode marks.

use crate::gate::Gate;
use crate::policy::{AppPolicy, Change, Policy, UserPolicy};
use crate::procfs::FileId;
use shisa_core::config::{
    App, Autostart, MAX_GRACE, MAX_PROMPT_TIMEOUT, MAX_PROTECTED_PATHS, MAX_RELOCK_SECONDS,
    MIN_PROMPT_TIMEOUT, UserConfig, valid_app_id,
};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::{error, info, warn};

const MAX_CONFIG_SIZE: u64 = 1 << 20;

/// Marked inodes and the path each was reached by.
pub type Marks = HashMap<FileId, PathBuf>;

/// The inode operations a reload needs. [`Gate`] performs the real fanotify
/// work. A reload's decisions are tested with a fake, because a real gate
/// needs `CAP_SYS_ADMIN`.
pub trait Marker {
    fn mark(&self, path: &Path) -> io::Result<FileId>;
    fn unmark(&self, path: &Path, id: FileId) -> io::Result<bool>;
}

impl Marker for Gate {
    fn mark(&self, path: &Path) -> io::Result<FileId> {
        Gate::mark(self, path)
    }

    fn unmark(&self, path: &Path, id: FileId) -> io::Result<bool> {
        Gate::unmark(self, path, id)
    }
}

impl<T: Marker + ?Sized> Marker for std::sync::Arc<T> {
    fn mark(&self, path: &Path) -> io::Result<FileId> {
        (**self).mark(path)
    }

    fn unmark(&self, path: &Path, id: FileId) -> io::Result<bool> {
        (**self).unmark(path, id)
    }
}

/// Config must be root's and not writable by anyone else, or an unprivileged
/// user could change what is protected (docs/subsystems/configuration.md).
fn check_owner(path: &Path, meta: &fs::Metadata) -> io::Result<()> {
    if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} must be owned by root and not writable by group or others",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn read_config(path: &Path) -> io::Result<String> {
    read_config_with(path, check_owner)
}

fn read_config_with(
    path: &Path,
    owner: fn(&Path, &fs::Metadata) -> io::Result<()>,
) -> io::Result<String> {
    let file = fs::File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    owner(path, &meta)?;
    // Truncating silently could cut at a line boundary and leave a smaller
    // but valid config, which would shrink protection without any error.
    if meta.len() > MAX_CONFIG_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("config larger than {MAX_CONFIG_SIZE} bytes"),
        ));
    }
    let mut text = String::new();
    file.take(MAX_CONFIG_SIZE).read_to_string(&mut text)?;
    Ok(text)
}

fn uid_of(name: &str) -> Option<u32> {
    shisa_core::users::uid(name)
}

/// The account name comes from NSS and must not reshape the path it is joined
/// onto. A name that is empty, `.`, `..`, has a slash, or has a NUL is refused.
fn config_file_name(name: &str) -> Result<String, String> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        return Err(format!("invalid account name {name:?}"));
    }
    Ok(format!("{name}.toml"))
}

/// Read one user's config for a settings change. A missing file means the
/// user has none yet, which is not an error here.
pub fn read_user(dir: &Path, name: &str) -> Result<UserConfig, String> {
    let path = dir.join(config_file_name(name)?);
    match read_config(&path) {
        Ok(text) => UserConfig::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(UserConfig::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Write one user's config as root: 0600, atomic rename. The policy lists the
/// apps and paths a user protects, which is not for other local accounts.
pub fn write_user(dir: &Path, name: &str, config: &UserConfig) -> io::Result<()> {
    write_user_with(dir, name, config, check_owner)
}

/// `owner` is a parameter so tests can write to a directory the current user
/// owns. Production always passes [`check_owner`].
fn write_user_with(
    dir: &Path,
    name: &str,
    config: &UserConfig,
    owner: fn(&Path, &fs::Metadata) -> io::Result<()>,
) -> io::Result<()> {
    let file =
        config_file_name(name).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    fs::create_dir_all(dir)?;
    // This file is written as root. A directory that is not root's, or that
    // another account can write, would let that account choose what gets
    // truncated. Check it before chmod, which would otherwise follow a
    // symlinked directory.
    owner(dir, &fs::metadata(dir)?)?;
    // The directory itself stays root-only. The daemon reads and writes it as
    // root, so no other account needs to list the per-user policy files.
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    let text = config
        .to_toml()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    let path = dir.join(&file);
    let temp = dir.join(format!(".{file}.new"));
    // A temp file left by a crash is removed before the retry, which also
    // removes a planted symlink instead of following it.
    match fs::remove_file(&temp) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    // A failed write must not leave a half-written temp file behind: the
    // next attempt would overwrite it, but it would sit there until then.
    // The data is synced before the rename. Otherwise a crash shortly after
    // could leave an empty file under the final name, and an empty config
    // parses as "nothing protected".
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .and_then(|mut file| {
            file.write_all(text.as_bytes())?;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&temp, &path));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

/// A path the daemon can actually mark: absolute, a regular file after
/// following symlinks, and not a shell or one of Shisa's own programs. A
/// directory, a missing file or the authentication agent itself would leave
/// the app listed as protected while nothing was watched, or lock
/// authentication out.
fn usable_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("not an absolute path".into());
    }
    if let Some(why) = shisa_core::desktop::unsuitable(path) {
        return Err(why.message().into());
    }
    match fs::metadata(path) {
        Ok(meta) if meta.is_file() => Ok(()),
        Ok(_) => Err("not a regular file".into()),
        Err(e) => Err(format!("cannot read: {e}")),
    }
}

/// Refuse values the daemon would not use as written. Checked when the
/// request arrives, so the user is not asked for a password first, and again
/// when the change is applied.
pub fn check_range(change: &Change) -> Result<(), String> {
    match change {
        Change::Grace(seconds) if *seconds > MAX_GRACE => {
            Err(format!("grace_seconds must be at most {MAX_GRACE}"))
        }
        Change::PromptTimeout(seconds)
            if !(MIN_PROMPT_TIMEOUT..=MAX_PROMPT_TIMEOUT).contains(seconds) =>
        {
            Err(format!(
                "prompt_timeout_seconds must be between {MIN_PROMPT_TIMEOUT} and {MAX_PROMPT_TIMEOUT}"
            ))
        }
        Change::Relock(seconds) if *seconds > MAX_RELOCK_SECONDS => Err(format!(
            "relock_after_unfocus_seconds must be at most {MAX_RELOCK_SECONDS}"
        )),
        // Cheap checks only: this runs on the bus thread. Whether a path is
        // a markable file is checked on the reload thread, after the
        // authentication.
        Change::Paths(paths) if paths.len() > MAX_PROTECTED_PATHS => {
            Err(format!("at most {MAX_PROTECTED_PATHS} protected paths"))
        }
        Change::Paths(paths) if paths.is_empty() => {
            Err("keep at least one protected path, or turn off protection instead".into())
        }
        Change::Paths(paths) if paths.iter().any(|p| !p.is_absolute()) => {
            Err("protected paths must be absolute".into())
        }
        _ => Ok(()),
    }
}

/// Apply an authenticated settings change to one user's config.
pub fn apply(
    config: &mut UserConfig,
    app: &str,
    change: &Change,
    resolve: impl FnOnce(&str) -> Result<Vec<PathBuf>, String>,
) -> Result<(), String> {
    fn entry<'a>(config: &'a mut UserConfig, app: &str) -> Result<&'a mut App, String> {
        config
            .apps
            .iter_mut()
            .find(|a| a.id == app)
            .ok_or_else(|| format!("{app} is not protected"))
    }
    check_range(change)?;
    match change {
        Change::Protected(true) => {
            if config.apps.iter().any(|a| a.id == app) {
                return Ok(());
            }
            if !valid_app_id(app) {
                return Err(format!("invalid app id {app:?}"));
            }
            let paths = resolve(app)?;
            if paths.is_empty() {
                return Err(format!("{app} has no usable protected path"));
            }
            if paths.len() > MAX_PROTECTED_PATHS {
                return Err(format!(
                    "{app} has more than {MAX_PROTECTED_PATHS} protected paths"
                ));
            }
            for path in &paths {
                usable_path(path)?;
            }
            config.apps.push(App {
                id: app.to_string(),
                paths,
                grace_seconds: None,
                autostart: Autostart::Prompt,
                relock_after_unfocus_seconds: 0,
            });
        }
        Change::Protected(false) => config.apps.retain(|a| a.id != app),
        Change::Grace(seconds) if app.is_empty() => config.global.grace_seconds = *seconds,
        Change::Grace(seconds) => entry(config, app)?.grace_seconds = Some(*seconds),
        Change::Autostart(on) => {
            entry(config, app)?.autostart = if *on {
                Autostart::Allow
            } else {
                Autostart::Prompt
            }
        }
        Change::Paths(paths) => {
            // An app without a path is listed as protected and stops nothing.
            // Giving up the protection has its own request, which the
            // authentication box names as such.
            if paths.is_empty() {
                return Err(format!(
                    "{app} must keep at least one protected path, or turn off protection instead"
                ));
            }
            for path in paths {
                usable_path(path)?;
            }
            entry(config, app)?.paths = paths.clone();
        }
        Change::ClearOnLock(on) => config.global.clear_on_screen_lock = *on,
        Change::PromptTimeout(seconds) => config.global.prompt_timeout_seconds = *seconds,
        Change::Relock(seconds) => {
            entry(config, app)?.relock_after_unfocus_seconds = *seconds;
        }
    }
    Ok(())
}

/// Read every `<username>.toml` in `dir`, mark the protected paths and return
/// the resulting policy. A user whose file cannot be read or parsed keeps the
/// entry from `old`, so a typo during a reload does not switch protection off.
/// The flag is true when any file or account lookup failed, so the daemon can
/// report it.
pub fn load(marker: &dyn Marker, dir: &Path, old: &Policy, marks: &mut Marks) -> (Policy, bool) {
    load_with(marker, dir, old, marks, check_owner)
}

/// Open the config directory after the owner check. A directory left by an
/// older package may still be group- or world-readable, so tighten it here as
/// well as on write. A read-only mount must not cost the policy, so a failed
/// chmod is logged and the load continues.
fn read_config_dir(
    dir: &Path,
    owner: fn(&Path, &fs::Metadata) -> io::Result<()>,
) -> io::Result<fs::ReadDir> {
    let meta = fs::metadata(dir)?;
    owner(dir, &meta)?;
    if meta.mode() & 0o077 != 0
        && let Err(e) = fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
    {
        warn!(
            "config directory {}: cannot set mode 0700: {e}",
            dir.display()
        );
    }
    fs::read_dir(dir)
}

/// `owner` is a parameter so tests can read a config the current user owns.
/// Production always passes [`check_owner`].
fn load_with(
    marker: &dyn Marker,
    dir: &Path,
    old: &Policy,
    marks: &mut Marks,
    owner: fn(&Path, &fs::Metadata) -> io::Result<()>,
) -> (Policy, bool) {
    let mut policy = Policy::default();
    let mut failed = false;
    let entries = match read_config_dir(dir, owner) {
        Ok(entries) => entries,
        Err(e) => {
            error!("config directory {}: {e}", dir.display());
            return (old.clone(), true);
        }
    };
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => files.push(entry.path()),
            Err(e) => {
                // Without this entry, a user's file may be skipped and the
                // protection it lists would go away. Treat it like an
                // unreadable directory and wait for the next reload.
                error!("config directory {}: {e}", dir.display());
                return (old.clone(), true);
            }
        }
    }
    files.sort();
    for path in files {
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(uid) = uid_of(name) else {
            // The account may be gone, or the lookup may have failed (NSS
            // down, a transient error). Without an answer the user is not
            // known to have no configuration, so keep what the previous
            // policy had for this name. Protection only grows here.
            match old.users.iter().find(|(_, user)| user.name == name) {
                Some((uid, previous)) => {
                    warn!("{name}: cannot resolve the account, keeping the previous policy");
                    policy.users.insert(*uid, previous.clone());
                }
                None => warn!("{}: no such user, ignored", path.display()),
            }
            failed = true;
            continue;
        };
        let parsed = read_config_with(&path, owner)
            .map_err(|e| e.to_string())
            .and_then(|text| UserConfig::parse(&text).map_err(|e| e.to_string()));
        let config = match parsed {
            Ok(config) => config,
            Err(e) => {
                error!("{}: {e}", path.display());
                if let Some(previous) = old.users.get(&uid) {
                    warn!("keeping the previous policy of {name}");
                    policy.users.insert(uid, previous.clone());
                }
                failed = true;
                continue;
            }
        };
        let mut apps = Vec::new();
        for app in &config.apps {
            let mut ids = Vec::new();
            let mut effective = Vec::new();
            for p in &app.paths {
                // A hand-written config can name a shell, a directory or a
                // file that is not there. Marking it would either fail or
                // break the session, so skip it and say why.
                if let Err(why) = usable_path(p) {
                    warn!("{name}: {}: skipping {}: {why}", app.id, p.display());
                    continue;
                }
                match marker.mark(p) {
                    Ok(id) => {
                        marks.insert(id, p.clone());
                        if !ids.contains(&id) {
                            ids.push(id);
                        }
                        if !effective.contains(p) {
                            effective.push(p.clone());
                        }
                    }
                    Err(e) => warn!("{name}: {}: cannot mark {}: {e}", app.id, p.display()),
                }
            }
            if ids.is_empty() {
                warn!("{name}: {} has no usable path, it covers nothing", app.id);
            }
            apps.push(AppPolicy {
                id: app.id.clone(),
                grace: Duration::from_secs(config.grace_seconds(app)),
                autostart: app.autostart == Autostart::Allow,
                relock: Duration::from_secs(
                    app.relock_after_unfocus_seconds.min(MAX_RELOCK_SECONDS),
                ),
                paths: app.paths.clone(),
                effective,
                files: ids,
            });
        }
        info!("{name}: {} protected apps", apps.len());
        let prompt_timeout = Duration::from_secs(config.prompt_timeout_seconds());
        let global_grace = Duration::from_secs(config.global_grace_seconds());
        policy.users.insert(
            uid,
            UserPolicy {
                name: name.to_string(),
                prompt_timeout,
                global_grace,
                clear_on_lock: config.global.clear_on_screen_lock,
                apps,
            },
        );
    }
    (policy, failed)
}

/// Every configured path that was marked still leads to its inode. A replaced
/// file, a deleted path or a mark that failed makes this false, and the reload
/// thread places the marks again (docs/subsystems/launch-gate.md).
///
/// A path that was already missing or unusable at the last load does not force
/// a full reload every time. The check still stats it, so a file that appears
/// later is found.
pub fn marks_are_current(policy: &Policy) -> bool {
    for user in policy.users.values() {
        for app in &user.apps {
            for path in &app.paths {
                let current = fs::metadata(path).ok().map(|m| FileId {
                    dev: m.dev(),
                    ino: m.ino(),
                });
                if current.is_some_and(|id| app.files.contains(&id)) {
                    continue;
                }
                // Not marked now. If it was marked at the last load, it was
                // replaced, removed or became unusable, which needs a reload.
                // If it is usable but unmarked, the mark failed and is
                // retried. A path that was already missing or unusable is
                // left alone until it changes.
                if app.effective.contains(path) || usable_path(path).is_ok() {
                    return false;
                }
            }
        }
    }
    true
}

/// Whether the periodic revalidation has to reload. A mark that failed or a
/// path that changed shows up in `marks_are_current`. A configuration that
/// could not be read or parsed keeps its user out of the policy, so the mark
/// check cannot see it, and the failure flag has to keep asking for a load
/// until the file is repaired.
pub fn needs_reload(policy: &Policy, config_failed: bool) -> bool {
    config_failed || !marks_are_current(policy)
}

/// Remove marks from inodes that no policy refers to any more.
pub fn drop_stale_marks(marker: &dyn Marker, marks: &mut Marks, keep: &HashSet<FileId>) {
    marks.retain(|id, path| {
        if keep.contains(id) {
            return true;
        }
        // A replaced or deleted file leaves its old inode out of reach, so
        // this can fail. Events from that inode are allowed because the
        // policy no longer lists it.
        let _ = marker.unmark(path, *id);
        false
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Change;

    fn sample() -> UserConfig {
        UserConfig::parse("[[apps]]\nid = \"app\"\npaths = [\"/usr/bin/app\"]\n").unwrap()
    }

    fn paths() -> Vec<PathBuf> {
        let dir = std::env::temp_dir().join(format!("shisad-paths-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("app");
        fs::write(&bin, "x").unwrap();
        vec![bin]
    }

    #[test]
    fn protects_and_unprotects() {
        let mut config = UserConfig::default();
        apply(
            &mut config,
            "app",
            &Change::Protected(true),
            |_| Ok(paths()),
        )
        .unwrap();
        assert_eq!(config.apps.len(), 1);
        assert_eq!(config.apps[0].paths, paths());
        // Enabling twice keeps one entry and resolves nothing again.
        apply(
            &mut config,
            "app",
            &Change::Protected(true),
            |_| unreachable!(),
        )
        .unwrap();
        assert_eq!(config.apps.len(), 1);
        apply(
            &mut config,
            "app",
            &Change::Protected(false),
            |_| unreachable!(),
        )
        .unwrap();
        assert!(config.apps.is_empty());
        // Disabling an app that is not protected is not an error.
        apply(
            &mut config,
            "app",
            &Change::Protected(false),
            |_| unreachable!(),
        )
        .unwrap();
    }

    #[test]
    fn replaced_or_missing_files_make_the_marks_stale() {
        let dir = std::env::temp_dir().join(format!("shisad-stale-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("bin");
        fs::write(&bin, "one").unwrap();
        let id = {
            let meta = fs::metadata(&bin).unwrap();
            FileId {
                dev: meta.dev(),
                ino: meta.ino(),
            }
        };
        let app = AppPolicy {
            id: "app".into(),
            grace: Duration::ZERO,
            autostart: false,
            relock: Duration::ZERO,
            paths: vec![bin.clone()],
            effective: vec![bin.clone()],
            files: vec![id],
        };
        let user = UserPolicy {
            name: "u".into(),
            prompt_timeout: Duration::ZERO,
            global_grace: Duration::ZERO,
            clear_on_lock: true,
            apps: vec![app.clone()],
        };
        let policy = Policy {
            users: HashMap::from([(1, user)]),
        };
        assert!(marks_are_current(&policy));
        // What a package upgrade does: write a new file, rename it into place.
        fs::write(dir.join("bin.new"), "two").unwrap();
        fs::rename(dir.join("bin.new"), &bin).unwrap();
        assert!(!marks_are_current(&policy));
        // A mark that failed leaves files empty and has to be retried even
        // while the path itself stays the same.
        let mut unmarked = app.clone();
        unmarked.files.clear();
        let mut user = policy.users[&1].clone();
        user.apps = vec![unmarked];
        let policy = Policy {
            users: HashMap::from([(1, user)]),
        };
        assert!(!marks_are_current(&policy));
        fs::remove_file(&bin).unwrap();
        assert!(!marks_are_current(&policy));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_path_that_was_already_missing_does_not_force_reloads() {
        let dir = std::env::temp_dir().join(format!("shisad-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("gone");
        let sub = dir.join("sub");
        fs::create_dir_all(&sub).unwrap();
        let policy = |path: PathBuf, effective: Vec<PathBuf>, files: Vec<FileId>| Policy {
            users: HashMap::from([(
                1,
                UserPolicy {
                    name: "u".into(),
                    prompt_timeout: Duration::ZERO,
                    global_grace: Duration::ZERO,
                    clear_on_lock: true,
                    apps: vec![AppPolicy {
                        id: "app".into(),
                        grace: Duration::ZERO,
                        autostart: false,
                        relock: Duration::ZERO,
                        paths: vec![path],
                        effective,
                        files,
                    }],
                },
            )]),
        };
        // Missing at the last load and still missing: no reload.
        assert!(marks_are_current(&policy(
            missing.clone(),
            Vec::new(),
            Vec::new()
        )));
        // It appears: a usable file that is not marked is found.
        fs::write(&missing, "x").unwrap();
        assert!(!marks_are_current(&policy(
            missing.clone(),
            Vec::new(),
            Vec::new()
        )));
        // A directory that was already unusable is treated the same.
        assert!(marks_are_current(&policy(
            sub.clone(),
            Vec::new(),
            Vec::new()
        )));
        // If the last load had marked this path, a change to a directory
        // needs one reload.
        assert!(!marks_are_current(&policy(
            sub.clone(),
            vec![sub.clone()],
            Vec::new()
        )));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_configuration_failure_keeps_asking_for_a_reload() {
        // A user whose file failed to load is missing from the policy, so
        // the mark check has nothing to examine. The failure flag has to
        // keep the periodic reload going until the file parses.
        let policy = Policy::default();
        assert!(!needs_reload(&policy, false));
        assert!(needs_reload(&policy, true));
    }

    #[test]
    fn paths_must_be_markable_files() {
        let dir = std::env::temp_dir().join(format!("shisad-usable-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("file");
        fs::write(&file, "x").unwrap();
        assert!(usable_path(&file).is_ok());
        let link = dir.join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(usable_path(&link).is_ok());
        assert!(usable_path(&dir).is_err());
        assert!(usable_path(&dir.join("missing")).is_err());
        assert!(usable_path(Path::new("relative")).is_err());
        assert!(usable_path(Path::new("/usr/bin/bash")).is_err());
        assert!(usable_path(Path::new("/usr/bin/shisa-settings")).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_without_a_usable_path() {
        let mut config = UserConfig::default();
        assert!(
            apply(&mut config, "app", &Change::Protected(true), |_| Ok(
                Vec::new()
            ))
            .is_err()
        );
        assert!(
            apply(&mut config, "app", &Change::Protected(true), |_| Err(
                "boom".into()
            ))
            .is_err()
        );
        assert!(config.apps.is_empty());
        assert!(
            apply(&mut config, "bad/id", &Change::Protected(true), |_| Ok(
                paths()
            ))
            .is_err()
        );
        let many: Vec<PathBuf> = (0..=MAX_PROTECTED_PATHS)
            .map(|i| PathBuf::from(format!("/usr/bin/app{i}")))
            .collect();
        assert!(apply(&mut config, "many", &Change::Protected(true), |_| Ok(many)).is_err());
    }

    #[test]
    fn options_touch_the_right_fields() {
        let mut config = sample();
        apply(&mut config, "", &Change::Grace(600), |_| unreachable!()).unwrap();
        assert_eq!(config.global.grace_seconds, 600);
        apply(&mut config, "app", &Change::Grace(30), |_| unreachable!()).unwrap();
        assert_eq!(config.apps[0].grace_seconds, Some(30));
        apply(
            &mut config,
            "app",
            &Change::Autostart(true),
            |_| unreachable!(),
        )
        .unwrap();
        assert_eq!(config.apps[0].autostart, Autostart::Allow);
        apply(&mut config, "app", &Change::Relock(300), |_| unreachable!()).unwrap();
        assert_eq!(config.apps[0].relock_after_unfocus_seconds, 300);
        assert!(check_range(&Change::Relock(MAX_RELOCK_SECONDS)).is_ok());
        assert!(check_range(&Change::Relock(MAX_RELOCK_SECONDS + 1)).is_err());
        let first = paths().pop().unwrap();
        let second = first.with_file_name("launch");
        fs::write(&second, "x").unwrap();
        apply(
            &mut config,
            "app",
            &Change::Paths(vec![first.clone(), second.clone()]),
            |_| unreachable!(),
        )
        .unwrap();
        assert_eq!(config.apps[0].paths, vec![first, second]);
        apply(
            &mut config,
            "",
            &Change::ClearOnLock(false),
            |_| unreachable!(),
        )
        .unwrap();
        assert!(!config.global.clear_on_screen_lock);
        apply(
            &mut config,
            "",
            &Change::PromptTimeout(45),
            |_| unreachable!(),
        )
        .unwrap();
        assert_eq!(config.global.prompt_timeout_seconds, 45);
        // Options for an app that is not protected are refused.
        assert!(apply(&mut config, "other", &Change::Grace(5), |_| unreachable!()).is_err());
        assert!(
            apply(
                &mut config,
                "app",
                &Change::Paths(vec!["relative".into()]),
                |_| unreachable!()
            )
            .is_err()
        );
        // The last path cannot be removed: that is what unprotecting is for.
        assert!(
            apply(
                &mut config,
                "app",
                &Change::Paths(Vec::new()),
                |_| unreachable!()
            )
            .is_err()
        );
        assert_eq!(config.apps[0].paths.len(), 2);
    }

    #[test]
    fn out_of_range_values_are_refused() {
        let mut config = sample();
        let before = config.clone();
        for change in [
            Change::Grace(MAX_GRACE + 1),
            Change::Grace(u64::MAX),
            Change::PromptTimeout(0),
            Change::PromptTimeout(MAX_PROMPT_TIMEOUT + 1),
        ] {
            assert!(
                apply(&mut config, "", &change, |_| unreachable!()).is_err(),
                "{change:?}"
            );
        }
        assert!(
            apply(
                &mut config,
                "app",
                &Change::Grace(u64::MAX),
                |_| unreachable!()
            )
            .is_err()
        );
        assert_eq!(config, before);
        let many: Vec<PathBuf> = (0..=MAX_PROTECTED_PATHS)
            .map(|i| PathBuf::from(format!("/usr/bin/app{i}")))
            .collect();
        assert!(check_range(&Change::Paths(many)).is_err());
        apply(
            &mut config,
            "",
            &Change::Grace(MAX_GRACE),
            |_| unreachable!(),
        )
        .unwrap();
        apply(
            &mut config,
            "",
            &Change::PromptTimeout(MIN_PROMPT_TIMEOUT),
            |_| unreachable!(),
        )
        .unwrap();
    }

    #[test]
    fn a_written_config_reads_back() {
        let dir = std::env::temp_dir().join(format!("shisad-write-ok-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write_user_with(&dir, "u", &sample(), permissive).unwrap();
        let text = fs::read_to_string(dir.join("u.toml")).unwrap();
        assert_eq!(UserConfig::parse(&text).unwrap(), sample());
        assert!(!dir.join(".u.toml.new").exists());
        assert_eq!(
            fs::metadata(dir.join("u.toml")).unwrap().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_write_leaves_no_temp_file() {
        let dir = std::env::temp_dir().join(format!("shisad-write-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // A directory where the config file should be makes the rename fail.
        fs::create_dir_all(dir.join("u.toml")).unwrap();
        assert!(write_user_with(&dir, "u", &sample(), permissive).is_err());
        assert!(!dir.join(".u.toml.new").exists(), "temp file left behind");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_name_that_escapes_the_config_directory_is_refused() {
        let root = std::env::temp_dir().join(format!("shisad-name-{}", std::process::id()));
        let dir = root.join("config");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&dir).unwrap();
        for name in ["", ".", "..", "../escape", "a/b", "with\0nul"] {
            assert!(config_file_name(name).is_err(), "{name:?}");
            assert!(read_user(&dir, name).is_err(), "{name:?}");
            assert!(write_user(&dir, name, &sample()).is_err(), "{name:?}");
        }
        assert_eq!(config_file_name("alice").unwrap(), "alice.toml");
        assert!(!root.join("escape.toml").exists());
        assert!(!dir.join("a").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn changes_round_trip_through_the_file_format() {
        let mut config = sample();
        apply(
            &mut config,
            "app",
            &Change::Autostart(true),
            |_| unreachable!(),
        )
        .unwrap();
        let text = config.to_toml().unwrap();
        assert_eq!(UserConfig::parse(&text).unwrap(), config);
    }

    struct FakeMarker {
        fail: bool,
        next: std::cell::Cell<u64>,
    }

    impl FakeMarker {
        fn new(fail: bool) -> Self {
            Self {
                fail,
                next: std::cell::Cell::new(10),
            }
        }
    }

    impl Marker for FakeMarker {
        fn mark(&self, _path: &Path) -> io::Result<FileId> {
            if self.fail {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "test marker refuses",
                ));
            }
            let ino = self.next.get();
            self.next.set(ino + 1);
            Ok(FileId { dev: 1, ino })
        }

        fn unmark(&self, _path: &Path, _id: FileId) -> io::Result<bool> {
            Ok(true)
        }
    }

    /// Tests own the config they write, which the production owner check
    /// rejects. Fail-closed tests therefore inject this check and leave the
    /// real one to `a_directory_not_owned_by_root_keeps_the_previous_policy`.
    fn permissive(_: &Path, _: &fs::Metadata) -> io::Result<()> {
        Ok(())
    }

    fn config_dir(tag: &str) -> (PathBuf, String, u32) {
        let dir = std::env::temp_dir().join(format!("shisad-load-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let uid = unsafe { libc::getuid() };
        let name = shisa_core::users::name(uid).expect("own user");
        (dir, name, uid)
    }

    fn write_config(dir: &Path, name: &str, bin: &Path) {
        fs::write(
            dir.join(format!("{name}.toml")),
            format!("[[apps]]\nid = \"app\"\npaths = [\"{}\"]\n", bin.display()),
        )
        .unwrap();
    }

    #[test]
    fn a_load_tightens_an_existing_config_directory() {
        let (dir, name, _) = config_dir("tighten");
        let bin = dir.join("bin");
        fs::write(&bin, "x").unwrap();
        write_config(&dir, &name, &bin);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        let _ = load_with(
            &FakeMarker::new(false),
            &dir,
            &Policy::default(),
            &mut Marks::new(),
            permissive,
        );
        assert_eq!(fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_config_keeps_the_previous_policy() {
        let (dir, name, uid) = config_dir("broken");
        let bin = dir.join("bin");
        fs::write(&bin, "x").unwrap();
        write_config(&dir, &name, &bin);
        let marker = FakeMarker::new(false);
        let mut marks = Marks::new();
        let (first, first_failed) =
            load_with(&marker, &dir, &Policy::default(), &mut marks, permissive);
        assert!(!first_failed);
        let before = &first.users[&uid];
        assert_eq!(before.apps[0].files, vec![FileId { dev: 1, ino: 10 }]);

        fs::write(dir.join(format!("{name}.toml")), "this is not toml").unwrap();
        let (second, second_failed) = load_with(&marker, &dir, &first, &mut marks, permissive);
        assert!(second_failed);
        assert!(
            second.users.contains_key(&uid),
            "a broken config dropped the previous policy"
        );
        let after = &second.users[&uid];
        assert_eq!(after.apps[0].id, "app");
        assert_eq!(after.apps[0].files, before.apps[0].files);
        assert_eq!(after.apps[0].effective, before.apps[0].effective);
        // The old inode stays marked, which is what keeps the app protected
        // until the config is fixed.
        let keep: HashSet<FileId> = second
            .users
            .values()
            .flat_map(|user| user.apps.iter().flat_map(|app| app.files.iter().copied()))
            .collect();
        drop_stale_marks(&marker, &mut marks, &keep);
        assert!(marks.contains_key(&FileId { dev: 1, ino: 10 }));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_config_keeps_the_previous_policy() {
        let (dir, name, uid) = config_dir("unreadable");
        let bin = dir.join("bin");
        fs::write(&bin, "x").unwrap();
        write_config(&dir, &name, &bin);
        let marker = FakeMarker::new(false);
        let (first, _) = load_with(
            &marker,
            &dir,
            &Policy::default(),
            &mut Marks::new(),
            permissive,
        );

        // A directory where the config file should be cannot be read as a
        // regular file, the same as a file whose permissions deny it.
        let path = dir.join(format!("{name}.toml"));
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        let (second, second_failed) =
            load_with(&marker, &dir, &first, &mut Marks::new(), permissive);
        assert!(second_failed);
        assert!(
            second.users.contains_key(&uid),
            "an unreadable config dropped the previous policy"
        );
        assert_eq!(second.users[&uid].apps[0].id, "app");
        assert_eq!(
            second.users[&uid].apps[0].files,
            first.users[&uid].apps[0].files
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_config_directory_that_cannot_be_read_keeps_the_previous_policy() {
        let (dir, name, uid) = config_dir("missing-dir");
        let bin = dir.join("bin");
        fs::write(&bin, "x").unwrap();
        write_config(&dir, &name, &bin);
        let marker = FakeMarker::new(false);
        let (first, _) = load_with(
            &marker,
            &dir,
            &Policy::default(),
            &mut Marks::new(),
            permissive,
        );

        let gone = dir.join("gone");
        let (second, second_failed) =
            load_with(&marker, &gone, &first, &mut Marks::new(), permissive);
        assert!(second_failed);
        assert!(
            second.users.contains_key(&uid),
            "an unreadable directory dropped the previous policy"
        );
        assert_eq!(second.users[&uid].apps[0].id, "app");
        assert_eq!(
            second.users[&uid].apps[0].files,
            first.users[&uid].apps[0].files
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_directory_not_owned_by_root_keeps_the_previous_policy() {
        // As root every temp directory passes the check, so the test would be
        // vacuous.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let (dir, name, uid) = config_dir("owner");
        let bin = dir.join("bin");
        fs::write(&bin, "x").unwrap();
        write_config(&dir, &name, &bin);
        let marker = FakeMarker::new(false);
        let (first, _) = load_with(
            &marker,
            &dir,
            &Policy::default(),
            &mut Marks::new(),
            permissive,
        );
        // The real check runs here, and a temp directory belongs to the user.
        let (second, second_failed) = load(&marker, &dir, &first, &mut Marks::new());
        assert!(second_failed);
        assert!(
            second.users.contains_key(&uid),
            "a config directory owned by the user dropped the previous policy"
        );
        assert_eq!(
            second.users[&uid].apps[0].files,
            first.users[&uid].apps[0].files
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_config_file_not_owned_by_root_is_refused() {
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let (dir, name, _) = config_dir("file-owner");
        let path = dir.join(format!("{name}.toml"));
        fs::write(&path, "").unwrap();
        let err = read_config_with(&path, check_owner).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_name_that_cannot_be_resolved_keeps_the_previous_policy() {
        let (dir, _, _) = config_dir("unknown-user");
        let name = "shisa-test-missing-user";
        // The file exists but no account has this name, which is the same
        // answer a failed lookup gives.
        fs::write(dir.join(format!("{name}.toml")), "").unwrap();
        let previous = UserPolicy {
            name: name.into(),
            prompt_timeout: Duration::from_secs(60),
            global_grace: Duration::from_secs(300),
            clear_on_lock: true,
            apps: vec![AppPolicy {
                id: "app".into(),
                grace: Duration::from_secs(300),
                autostart: false,
                relock: Duration::ZERO,
                paths: vec!["/usr/bin/app".into()],
                effective: vec!["/usr/bin/app".into()],
                files: vec![FileId { dev: 1, ino: 10 }],
            }],
        };
        let uid = 4242;
        let old = Policy {
            users: HashMap::from([(uid, previous)]),
        };
        let marker = FakeMarker::new(false);
        let (policy, failed) = load_with(&marker, &dir, &old, &mut Marks::new(), permissive);
        assert!(failed);
        assert!(
            policy.users.contains_key(&uid),
            "an unresolvable account dropped the previous policy"
        );
        assert_eq!(policy.users[&uid].apps[0].id, "app");
        assert_eq!(
            policy.users[&uid].apps[0].files,
            vec![FileId { dev: 1, ino: 10 }]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_config_file_means_no_configuration() {
        let (dir, name, uid) = config_dir("no-file");
        let bin = dir.join("bin");
        fs::write(&bin, "x").unwrap();
        write_config(&dir, &name, &bin);
        let marker = FakeMarker::new(false);
        let (first, _) = load_with(
            &marker,
            &dir,
            &Policy::default(),
            &mut Marks::new(),
            permissive,
        );
        assert!(first.users.contains_key(&uid));
        fs::remove_file(dir.join(format!("{name}.toml"))).unwrap();
        // Unlike a read or parse error, an absent file is a user who has no
        // configuration, so the previous entry is not kept.
        let (second, second_failed) =
            load_with(&marker, &dir, &first, &mut Marks::new(), permissive);
        assert!(!second_failed);
        assert!(!second.users.contains_key(&uid));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_mark_keeps_the_app_listed_but_unmarked() {
        let (dir, name, uid) = config_dir("mark-fail");
        let bin = dir.join("bin");
        fs::write(&bin, "x").unwrap();
        write_config(&dir, &name, &bin);
        let (policy, failed) = load_with(
            &FakeMarker::new(true),
            &dir,
            &Policy::default(),
            &mut Marks::new(),
            permissive,
        );
        // The configuration itself read, so only the mark failure is reported.
        assert!(!failed);
        let app = &policy.users[&uid].apps[0];
        // Settings must still show the path, while `effective` says nothing
        // is watched.
        assert_eq!(app.paths, vec![bin]);
        assert!(app.files.is_empty());
        assert!(app.effective.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
