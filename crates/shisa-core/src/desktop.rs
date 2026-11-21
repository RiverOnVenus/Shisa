//! Desktop file lookup and protected path resolution (docs/subsystems/launch-gate.md).
//!
//! The settings UI and `shisa protect` both go through this. Everything here
//! is read-only and works on injected directories, so the tests do not need
//! the real /usr/share.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::i18n::Language;

/// Where desktop files, executables and Flatpak deployments are looked up.
#[derive(Debug, Clone)]
pub struct Env {
    /// XDG data directories, in search order.
    pub data_dirs: Vec<PathBuf>,
    /// Directory list used to resolve a bare command name.
    pub path: Vec<PathBuf>,
    /// Flatpak installation roots, each containing `app/<id>/current/active`.
    pub flatpak_roots: Vec<PathBuf>,
    /// Set when another process reads on behalf of this account: its uid and
    /// home directory. Files below the home must then belong to the account.
    pub owner: Option<(u32, PathBuf)>,
    /// The account's XDG config directory, for the interface language.
    pub config_home: PathBuf,
    /// Locale for `Name[locale]` keys and the interface, from `LC_ALL`,
    /// `LC_MESSAGES` or `LANG`.
    pub locale: Option<String>,
}

/// Desktop files and Flatpak metadata are small. Anything larger is not one.
const MAX_FILE_SIZE: u64 = 1 << 20;
/// Bounds for one scan, so a user cannot make the daemon walk or read
/// without limit through their own applications directories.
const MAX_ENTRIES: usize = 8192;
const MAX_SCAN_BYTES: usize = 8 << 20;

impl Env {
    pub fn current() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        // The XDG spec says an empty value means unset, and a relative data
        // directory is invalid and ignored.
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".local/share"));
        let data_dirs = std::env::var_os("XDG_DATA_DIRS")
            .filter(|v| !v.is_empty())
            .and_then(|v| v.into_string().ok())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        let path = std::env::var_os("PATH")
            .and_then(|v| v.into_string().ok())
            .unwrap_or_default();
        let config_home = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".config"));
        let locale = std::env::var("LC_ALL")
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| std::env::var("LC_MESSAGES").ok().filter(|v| !v.is_empty()))
            .or_else(|| std::env::var("LANG").ok().filter(|v| !v.is_empty()));
        let mut env = Env::from_dirs(data_home, &data_dirs, &path, &home, config_home);
        env.locale = locale;
        env
    }

    /// The environment of an account, for work shisad does on its behalf.
    /// The session's XDG directories, PATH and locale come from the user's
    /// systemd manager, which is where Plasma puts them. Without that, the
    /// standard directories are used.
    pub fn for_user(uid: u32) -> Option<Self> {
        let home = crate::users::home(uid)?;
        let vars = session_env(uid);
        let data_home = vars
            .get("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".local/share"));
        let data_dirs = vars
            .get("XDG_DATA_DIRS")
            .filter(|v| !v.is_empty())
            .map(String::as_str)
            .unwrap_or("/usr/local/share:/usr/share");
        let path = vars
            .get("PATH")
            .filter(|v| !v.is_empty())
            .map(String::as_str)
            .unwrap_or("/usr/local/bin:/usr/bin:/bin");
        let config_home = vars
            .get("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".config"));
        let mut env = Env::from_dirs(data_home, data_dirs, path, &home, config_home);
        env.locale = vars
            .get("LC_ALL")
            .filter(|v| !v.is_empty())
            .or_else(|| vars.get("LC_MESSAGES").filter(|v| !v.is_empty()))
            .or_else(|| vars.get("LANG").filter(|v| !v.is_empty()))
            .cloned();
        env.owner = Some((uid, home));
        Some(env)
    }

    /// The interface language for the account this environment belongs to.
    pub fn language(&self) -> Language {
        crate::i18n::for_account(&self.config_home, self.locale.as_deref())
    }

    /// Read a desktop file or Flatpak metadata file as text.
    ///
    /// shisad calls this as root for files the account controls, so it only
    /// accepts a regular file of limited size, opened without blocking (a
    /// FIFO in the applications directory must not stall the caller). Below
    /// the account's home the file has to be owned by the account: a link
    /// there may point anywhere, and root must not read on the account's
    /// behalf what the account could not read itself.
    fn read(&self, path: &Path) -> io::Result<String> {
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
            .open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.len() > MAX_FILE_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a small regular file",
            ));
        }
        if let Some((uid, home)) = &self.owner
            && path.starts_with(home)
            && meta.uid() != *uid
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "not owned by the account",
            ));
        }
        let mut text = String::new();
        file.take(MAX_FILE_SIZE).read_to_string(&mut text)?;
        // A NUL cannot travel in a D-Bus string, and a file with one is not a
        // desktop file the daemon should trust.
        if text.contains('\0') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "contains a NUL byte",
            ));
        }
        Ok(text)
    }

    fn from_dirs(
        data_home: PathBuf,
        data_dirs: &str,
        path: &str,
        home: &Path,
        config_home: PathBuf,
    ) -> Self {
        let mut dirs = vec![data_home];
        for dir in data_dirs
            .split(':')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            // Relative entries are invalid per the XDG spec.
            .filter(|p| p.is_absolute())
        {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        let flatpak_roots = vec![
            home.join(".local/share/flatpak"),
            PathBuf::from("/var/lib/flatpak"),
        ];
        // Flatpak puts the desktop files of installed apps into exports/share.
        // A session's XDG_DATA_DIRS usually lists these already; the duplicate
        // is dropped.
        for root in &flatpak_roots {
            let dir = root.join("exports/share");
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        Env {
            data_dirs: dirs,
            path: path
                .split(':')
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .collect(),
            flatpak_roots,
            owner: None,
            config_home,
            locale: None,
        }
    }
}

/// The environment of the user's systemd manager, which carries the session's
/// XDG directories, PATH and locale. The daemon itself has none of them.
fn session_env(uid: u32) -> HashMap<String, String> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return HashMap::new();
    };
    let unit = format!("user@{uid}.service");
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
            .is_none()
        {
            continue;
        }
        let dir = entry.path();
        let Ok(comm) = fs::read_to_string(dir.join("comm")) else {
            continue;
        };
        if comm.trim() != "systemd" {
            continue;
        }
        let Ok(cgroup) = fs::read_to_string(dir.join("cgroup")) else {
            continue;
        };
        if !cgroup.contains(&unit) {
            continue;
        }
        let Ok(environ) = fs::read(dir.join("environ")) else {
            continue;
        };
        let mut vars = HashMap::new();
        for pair in environ.split(|b| *b == 0) {
            let Some(eq) = pair.iter().position(|b| *b == b'=') else {
                continue;
            };
            let key = String::from_utf8_lossy(&pair[..eq]).into_owned();
            let value = String::from_utf8_lossy(&pair[eq + 1..]).into_owned();
            vars.insert(key, value);
        }
        return vars;
    }
    HashMap::new()
}

/// One row of the settings application list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledApp {
    pub id: String,
    pub name: String,
    /// Icon theme name for the row.
    pub icon: String,
}

/// What the resolution found, ready to be shown in the settings window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub desktop_file: PathBuf,
    pub name: String,
    /// Files to mark, canonical and deduplicated by inode.
    pub paths: Vec<PathBuf>,
    /// Caveats the settings window has to show (docs/subsystems/launch-gate.md).
    pub notes: Vec<Note>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    /// The binary is used by other applications too, so marking it would lock
    /// them as well. Only the wrapper script is protected.
    SharedRuntime(PathBuf),
    /// The Flatpak entry binary could not be found.
    FlatpakEntryMissing(String),
    /// The command could not be found at all.
    CommandNotFound(String),
}

impl Note {
    /// The caveat in the interface language. shisad formats these for the
    /// caller because the settings window and the CLI show them as text.
    pub fn message(&self, language: Language) -> String {
        match (self, language) {
            (Note::SharedRuntime(p), Language::ZhCn) => format!(
                "{} 被多个应用共用，不标记。直接调用运行时不经过包装脚本，覆盖不到。",
                p.display()
            ),
            (Note::SharedRuntime(p), Language::En) => format!(
                "{} is shared with other applications, so it is not marked. Calling the runtime directly bypasses the wrapper script and is not covered.",
                p.display()
            ),
            (Note::FlatpakEntryMissing(id), Language::ZhCn) => {
                format!("{id} 的入口二进制没找到，覆盖不到。")
            }
            (Note::FlatpakEntryMissing(id), Language::En) => {
                format!("The entry binary for {id} was not found, so it is not covered.")
            }
            (Note::CommandNotFound(c), Language::ZhCn) => {
                format!("找不到命令 {c}，覆盖不到。")
            }
            (Note::CommandNotFound(c), Language::En) => {
                format!("The command {c} was not found, so it is not covered.")
            }
        }
    }
}

#[derive(Debug)]
pub enum Error {
    NotFound,
    Io(io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotFound => f.write_str("desktop file not found"),
            Error::Io(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// Every installed application, sorted by name. The first desktop file for an
/// id wins, in data directory order.
pub fn installed(env: &Env) -> Vec<InstalledApp> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut bytes = 0usize;
    for (id, path) in all_desktop_files(env) {
        if !seen.insert(id.clone()) {
            continue;
        }
        let Ok(text) = env.read(&path) else { continue };
        bytes += text.len();
        if bytes > MAX_SCAN_BYTES {
            break;
        }
        if !is_application(&text) {
            continue;
        }
        out.push(InstalledApp {
            name: desktop_name(&text, env.locale.as_deref()),
            icon: desktop_icon(&text),
            id,
        });
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

/// The name to show for one app, or the id when its desktop file is missing
/// or hidden. Used for the authentication dialog.
pub fn display_name(app_id: &str, env: &Env) -> String {
    for (id, path) in all_desktop_files(env) {
        if id != app_id {
            continue;
        }
        let Ok(text) = env.read(&path) else { break };
        // The first file for an id wins, as in `installed`.
        if is_hidden(&text) {
            break;
        }
        let name = desktop_name(&text, env.locale.as_deref());
        return if name.is_empty() {
            app_id.to_string()
        } else {
            name
        };
    }
    app_id.to_string()
}

/// `Icon` from the main group.
fn desktop_icon(text: &str) -> String {
    let mut group = String::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            group = name.to_string();
            continue;
        }
        if group != "Desktop Entry" {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim() == "Icon"
        {
            return decode_escapes(value.trim());
        }
    }
    String::new()
}

/// `Type` must be Application, or be absent with an Exec line present.
/// Entries marked NoDisplay or Hidden are not shown in menus, so the settings
/// list leaves them out too.
fn is_application(text: &str) -> bool {
    let mut group = String::new();
    let (mut kind, mut has_exec) = (None::<String>, false);
    let (mut no_display, mut hidden) = (false, false);
    for line in text.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            group = name.to_string();
            continue;
        }
        if group != "Desktop Entry" {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            match key.trim() {
                "Type" => kind = Some(value.trim().to_string()),
                "Exec" => has_exec = true,
                "NoDisplay" => no_display = value.trim() == "true",
                "Hidden" => hidden = value.trim() == "true",
                _ => {}
            }
        }
    }
    kind.as_deref().map_or(has_exec, |k| k == "Application") && has_exec && !no_display && !hidden
}

/// One pass over the desktop files: where each id lives and which binaries
/// are shared runtimes. Building it reads every desktop file, so a caller
/// that resolves several apps in a row keeps it around.
pub struct Index {
    files: Vec<(String, PathBuf)>,
    shared: HashSet<PathBuf>,
}

impl Index {
    pub fn scan(env: &Env) -> Self {
        let files = all_desktop_files(env);
        let shared = shared_commands(&files, env);
        Index { files, shared }
    }
}

/// Resolve every `Exec` line of an installed application.
pub fn resolve(app_id: &str, env: &Env) -> Result<Resolved, Error> {
    resolve_in(&Index::scan(env), app_id, env)
}

/// `Hidden=true` removes an entry, including one of the same id further down
/// the data directory list, so a lookup that finds one stops there.
fn is_hidden(text: &str) -> bool {
    let mut group = String::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            group = name.to_string();
            continue;
        }
        if group != "Desktop Entry" {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim() == "Hidden"
            && value.trim() == "true"
        {
            return true;
        }
    }
    false
}

/// [`resolve`] against an index that was built earlier.
pub fn resolve_in(index: &Index, app_id: &str, env: &Env) -> Result<Resolved, Error> {
    let found = index.files.iter().find(|(id, _)| id == app_id);
    let desktop_file = found.map(|(_, path)| path.clone()).ok_or(Error::NotFound)?;
    let text = env.read(&desktop_file)?;
    if is_hidden(&text) {
        return Err(Error::NotFound);
    }
    let shared = &index.shared;
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    let mut notes = Vec::new();
    for exec in exec_lines(&text) {
        let args = split_exec(&exec);
        let args = skip_env(&args);
        let Some(command) = args.first() else {
            continue;
        };
        let file = match resolve_command(args, command, env) {
            Command::File(file) => file,
            Command::Missing => {
                let note = Note::CommandNotFound(command.clone());
                if !notes.contains(&note) {
                    notes.push(note);
                }
                continue;
            }
            Command::FlatpakEntryMissing(id) => {
                let note = Note::FlatpakEntryMissing(id);
                if !notes.contains(&note) {
                    notes.push(note);
                }
                continue;
            }
        };
        if shared.contains(&file) {
            let note = Note::SharedRuntime(file);
            if !notes.contains(&note) {
                notes.push(note);
            }
            continue;
        }
        let id = file_id(&file)?;
        if seen.insert(id) {
            paths.push(file);
        }
    }
    Ok(Resolved {
        desktop_file,
        name: desktop_name(&text, env.locale.as_deref()),
        paths,
        notes,
    })
}

/// `Name` from the main group, preferring a key for the current locale. The
/// spec asks clients to try `Name[lang_COUNTRY]`, then `Name[lang]`, then
/// `Name`.
fn desktop_name(text: &str, locale: Option<&str>) -> String {
    let wanted = locale_keys(locale);
    let mut default = None;
    let mut localized: HashMap<String, String> = HashMap::new();
    let mut group = String::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            group = name.to_string();
            continue;
        }
        if group != "Desktop Entry" {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "Name" => default = Some(value.trim().to_string()),
            key => {
                if let Some(loc) = key.strip_prefix("Name[").and_then(|k| k.strip_suffix(']')) {
                    localized.insert(loc.to_string(), value.trim().to_string());
                }
            }
        }
    }
    wanted
        .into_iter()
        .find_map(|key| localized.get(&key).cloned())
        .map(|v| decode_escapes(&v))
        .or_else(|| default.map(|v| decode_escapes(&v)))
        .unwrap_or_default()
}

/// Locale keys to try, most specific first. `zh_CN.UTF-8` becomes `zh_CN`,
/// then `zh`.
fn locale_keys(locale: Option<&str>) -> Vec<String> {
    let Some(locale) = locale else {
        return Vec::new();
    };
    let base = locale.split(['.', '@']).next().unwrap_or(locale);
    let mut keys = vec![base.to_string()];
    if let Some(lang) = base.split('_').next()
        && lang != base
    {
        keys.push(lang.to_string());
    }
    keys
}

/// All `Exec` values of the main group and the actions.
fn exec_lines(text: &str) -> Vec<String> {
    let mut group = String::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            group = name.to_string();
            continue;
        }
        if group != "Desktop Entry" && !group.starts_with("Desktop Action ") {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim() == "Exec"
        {
            out.push(value.trim().to_string());
        }
    }
    out
}

/// The general string escapes of the Desktop Entry spec, which apply to
/// every value before any other rule.
fn decode_escapes(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Field codes are replaced by the launcher, so they are not arguments. `%%`
/// is a literal percent sign.
fn strip_field_codes(arg: &str) -> String {
    let mut out = String::with_capacity(arg.len());
    let mut chars = arg.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some('f' | 'F' | 'u' | 'U' | 'i' | 'c' | 'k') => {}
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// Split an Exec line into arguments, per the desktop entry specification:
/// general escapes are decoded first, arguments are separated by whitespace,
/// and double quotes group them. Inside quotes only `\"`, `` \` ``, `\$` and
/// `\\` are escapes. Field codes are dropped.
pub fn split_exec(exec: &str) -> Vec<String> {
    let decoded = decode_escapes(exec);
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    let mut chars = decoded.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            '\\' if quoted => match chars.peek() {
                Some(&next) if matches!(next, '"' | '`' | '$' | '\\') => {
                    chars.next();
                    current.push(next);
                }
                _ => current.push('\\'),
            },
            c if c.is_whitespace() && !quoted => {
                if started {
                    push_arg(&mut args, std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        push_arg(&mut args, current);
    }
    args
}

/// An argument that consisted only of field codes disappears, while a quoted
/// empty argument stays.
fn push_arg(args: &mut Vec<String>, raw: String) {
    let stripped = strip_field_codes(&raw);
    if !stripped.is_empty() || raw.is_empty() {
        args.push(stripped);
    }
}

/// Why a program cannot be a protected path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsuitable {
    /// Shisa's own programs ask for the password, so marking them would lock
    /// authentication out.
    OwnProgram,
    /// A shell, interpreter or system program whose every exec would need a
    /// password, which would affect the whole session.
    SystemProgram,
}

impl Unsuitable {
    /// The English error text. Clients translate it for the Chinese
    /// interface, so the daemon keeps one canonical wording.
    pub fn message(self) -> &'static str {
        match self {
            Self::OwnProgram => {
                "Shisa's own programs cannot be protected, because authentication would be impossible"
            }
            Self::SystemProgram => {
                "protecting a shell, interpreter or system program would affect the whole session"
            }
        }
    }
}

/// Programs that must not become a protected path, and why.
pub fn unsuitable(path: &Path) -> Option<Unsuitable> {
    const OWN: &[&str] = &["shisa", "shisad", "shisa-agent", "shisa-settings"];
    const GENERAL: &[&str] = &[
        "sh",
        "bash",
        "dash",
        "ash",
        "zsh",
        "fish",
        "ksh",
        "csh",
        "tcsh",
        "rbash",
        "busybox",
        "env",
        "sudo",
        "pkexec",
        "su",
        "login",
        "expect",
        "systemd",
        "systemd-executor",
        "systemd-run",
        "dbus-daemon",
        "dbus-broker",
        "dbus-broker-launch",
        "python",
        "perl",
        "ruby",
        "node",
        "deno",
        "bun",
        "lua",
        "luajit",
        "php",
        "tclsh",
        "wish",
        "java",
        "flatpak",
        "bwrap",
        "xdg-open",
        "kioclient",
        "kwin_wayland",
        "plasmashell",
        "ld-linux-x86-64.so.2",
        "pkttyagent",
        "polkit-kde-authentication-agent-1",
    ];
    let real = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let name = real.file_name()?.to_str()?;
    // python3, python3.13 and the like count as their base name.
    let base = name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    if OWN.contains(&name) {
        Some(Unsuitable::OwnProgram)
    } else if GENERAL.contains(&name) || GENERAL.contains(&base) {
        Some(Unsuitable::SystemProgram)
    } else {
        None
    }
}

/// `Exec=env VAR=value program ...` starts the program, not env. Drop the
/// `env` word, its options and the assignments, so the program is what gets
/// resolved. Marking /usr/bin/env itself would be useless.
fn skip_env(args: &[String]) -> &[String] {
    let is_env = args
        .first()
        .is_some_and(|a| Path::new(a).file_name().is_some_and(|n| n == "env"));
    if !is_env {
        return args;
    }
    let mut i = 1;
    while let Some(arg) = args.get(i) {
        if arg == "--" {
            i += 1;
            break;
        }
        // Options that take their value as the next word.
        if matches!(arg.as_str(), "-u" | "--unset" | "-C" | "--chdir") {
            i += 2;
        } else if arg.starts_with('-') || (arg.contains('=') && !arg.starts_with('/')) {
            i += 1;
        } else {
            break;
        }
    }
    args.get(i..).unwrap_or(&[])
}

enum Command {
    File(PathBuf),
    Missing,
    FlatpakEntryMissing(String),
}

/// Find the executable behind the first argument of an Exec line.
fn resolve_command(args: &[String], command: &str, env: &Env) -> Command {
    let found = if command.contains('/') {
        canonical_file(Path::new(command))
    } else {
        env.path
            .iter()
            .map(|dir| dir.join(command))
            .find_map(|p| canonical_file(&p))
    };
    let Some(file) = found else {
        return Command::Missing;
    };
    // `flatpak run <id>` names the shared flatpak binary. The application
    // itself is the entry inside its deployment. The `run` subcommand may
    // follow global options such as `--user`.
    let runs_flatpak = flatpak_run(args).is_some();
    if runs_flatpak && file.file_name().is_some_and(|n| n == "flatpak") {
        return match flatpak_entry(args, env) {
            Some(entry) => Command::File(entry),
            None => Command::FlatpakEntryMissing(flatpak_id(args).unwrap_or_default()),
        };
    }
    Command::File(file)
}

/// The position of the `run` subcommand in a `flatpak` command line, after
/// any global options. None when this is not a `flatpak run` invocation.
fn flatpak_run(args: &[String]) -> Option<usize> {
    // Global options that take their value as the next word.
    const VALUED: &[&str] = &["--installation", "--arch"];
    let mut i = 1;
    while let Some(arg) = args.get(i) {
        if arg == "run" {
            return Some(i);
        }
        if arg == "--" {
            i += 1;
        } else if VALUED.contains(&arg.as_str()) {
            i += 2;
        } else if arg.starts_with('-') {
            i += 1;
        } else {
            return None;
        }
    }
    None
}

fn flatpak_id(args: &[String]) -> Option<String> {
    // Options that take their value as the following word would otherwise
    // pass a branch or architecture name off as the application id.
    const VALUED: &[&str] = &[
        "--branch",
        "--arch",
        "--command",
        "--installation",
        "--app-path",
        "--runtime",
        "--runtime-version",
    ];
    let mut i = flatpak_run(args)? + 1;
    while let Some(arg) = args.get(i) {
        if arg == "--" {
            i += 1;
            break;
        }
        if VALUED.contains(&arg.as_str()) {
            i += 2;
        } else if arg.starts_with('-') {
            i += 1;
        } else {
            break;
        }
    }
    let id = args.get(i)?;
    valid_flatpak_id(id).then(|| id.clone())
}

/// A reverse-DNS application id with at least three elements. Rejecting
/// anything else keeps `.` and `..` from reaching a path.
fn valid_flatpak_id(id: &str) -> bool {
    let parts: Vec<&str> = id.split('.').collect();
    parts.len() >= 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
}

fn flatpak_entry(args: &[String], env: &Env) -> Option<PathBuf> {
    let id = flatpak_id(args)?;
    for root in &env.flatpak_roots {
        let base = root.join("app").join(&id).join("current/active");
        let Ok(metadata) = env.read(&base.join("metadata")) else {
            continue;
        };
        let Some(command) = metadata_command(&metadata) else {
            continue;
        };
        // The metadata is user-writable. The command names a file inside the
        // deployment, never an absolute or escaping path.
        let command = Path::new(&command);
        if command.is_absolute()
            || command
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            continue;
        }
        let bin_dir = base.join("files/bin");
        let (Ok(real_bin), Ok(real_entry)) = (
            fs::canonicalize(&bin_dir),
            fs::canonicalize(bin_dir.join(command)),
        ) else {
            continue;
        };
        if !real_entry.starts_with(&real_bin) || !real_entry.is_file() {
            continue;
        }
        // Not canonicalized on purpose: `current/active` points at the
        // deployed commit, and the canonical path dies with the next update.
        // The mark follows the links, so a reload finds the new binary.
        let entry = bin_dir.join(command);
        return Some(entry);
    }
    None
}

/// The entry command from the `[Application]` group of a Flatpak metadata
/// file. Flatpak writes the keys in lower case.
fn metadata_command(text: &str) -> Option<String> {
    let mut group = String::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            group = name.to_string();
            continue;
        }
        if group != "Application" {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case("command")
            && let Some(word) = split_exec(value.trim()).first()
        {
            return Some(word.clone());
        }
    }
    None
}

fn canonical_file(path: &Path) -> Option<PathBuf> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    fs::canonicalize(path).ok()
}

fn file_id(path: &Path) -> io::Result<(u64, u64)> {
    let meta = fs::metadata(path)?;
    Ok((meta.dev(), meta.ino()))
}

/// Executables that several applications name as their command. Marking one
/// would lock every application using it.
///
/// The first desktop file for an id wins, exactly as in [`resolve_in`] and
/// [`installed`]: a user copy in `~/.local/share/applications` shadows the
/// system file, and counting both would pass one application off as two.
///
/// A binary is shared when two different applications make it run different
/// things. One application often ships several entries (`codium` and
/// `codium-wayland`) and more `Exec` lines in its actions, and those share
/// the protected path (docs/subsystems/launch-gate.md), so every entry of one id counts as that
/// one application. Electron apps pass different `app.asar` files and a
/// launcher gets different `env` assignments, so those differ.
fn shared_commands(files: &[(String, PathBuf)], env: &Env) -> HashSet<PathBuf> {
    let mut targets: HashMap<PathBuf, HashMap<&str, HashSet<String>>> = HashMap::new();
    let mut seen = HashSet::new();
    let mut bytes = 0usize;
    for (id, path) in files {
        if !seen.insert(id.as_str()) {
            continue;
        }
        let Ok(text) = env.read(path) else { continue };
        bytes += text.len();
        if bytes > MAX_SCAN_BYTES {
            break;
        }
        if is_hidden(&text) {
            continue;
        }
        for exec in exec_lines(&text) {
            let all = split_exec(&exec);
            let args = skip_env(&all);
            let Some(command) = args.first() else {
                continue;
            };
            if let Command::File(file) = resolve_command(args, command, env) {
                targets
                    .entry(file)
                    .or_default()
                    .entry(id.as_str())
                    .or_default()
                    .insert(target_of(&all, args));
            }
        }
    }
    targets
        .into_iter()
        .filter(|(_, apps)| {
            let mut sets = apps.values();
            let Some(first) = sets.next() else {
                return false;
            };
            apps.len() > 1 && sets.any(|set| set != first)
        })
        .map(|(file, _)| file)
        .collect()
}

/// What an Exec line makes its command run: the `env` assignments in front of
/// it and the first argument that is neither an option nor a field code.
/// Empty for a plain `program --flags %U`.
fn target_of(all: &[String], args: &[String]) -> String {
    let assignments = &all[..all.len() - args.len()];
    let mut parts: Vec<&str> = assignments
        .iter()
        .filter(|a| a.contains('='))
        .map(String::as_str)
        .collect();
    let positional = args
        .iter()
        .skip(1)
        .find(|a| !a.starts_with('-') && !a.starts_with('%'));
    parts.extend(positional.map(String::as_str));
    parts.join(" ")
}

fn all_desktop_files(env: &Env) -> Vec<(String, PathBuf)> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>, depth: u8) {
        if out.len() >= MAX_ENTRIES {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if out.len() >= MAX_ENTRIES {
                return;
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            // Do not follow a symlinked directory out of the applications
            // directory. A symlinked desktop file itself is still read.
            let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
            if is_dir {
                if depth < 3 {
                    walk(&path, &format!("{prefix}{name}-"), out, depth + 1);
                }
            } else if let Some(id) = name.strip_suffix(".desktop") {
                out.push((format!("{prefix}{id}"), path));
            }
        }
    }
    let mut out = Vec::new();
    for dir in &env.data_dirs {
        walk(&dir.join("applications"), "", &mut out, 0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct Fixture {
        dir: PathBuf,
        env: Env,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn fixture(name: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("shisa-desktop-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let data = dir.join("share");
        let bin = dir.join("bin");
        fs::create_dir_all(data.join("applications")).unwrap();
        fs::create_dir_all(&bin).unwrap();
        // Built directly rather than through from_dirs: that adds the host's
        // Flatpak exports, and their apps would leak into these assertions.
        let env = Env {
            data_dirs: vec![data],
            path: vec![bin],
            flatpak_roots: vec![dir.join("home/.local/share/flatpak")],
            owner: None,
            config_home: dir.join("home/.config"),
            locale: None,
        };
        Fixture { dir, env }
    }

    fn app(fx: &Fixture, id: &str, body: &str) {
        let path = fx.env.data_dirs[0]
            .join("applications")
            .join(format!("{id}.desktop"));
        fs::write(path, body).unwrap();
    }

    fn binary(fx: &Fixture, name: &str) -> PathBuf {
        let path = fx.dir.join(name);
        fs::write(&path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn absolute_and_path_commands() {
        let fx = fixture("basic");
        let abs = binary(&fx, "abs-bin");
        let rel = binary(&fx, "rel-bin");
        fs::rename(&rel, fx.env.path[0].join("rel-bin")).unwrap();
        app(
            &fx,
            "test.one",
            &format!(
                "[Desktop Entry]\nName=One\nExec={} %U\n[Desktop Action new]\nName=New\nExec={} --new\n",
                abs.display(),
                "rel-bin"
            ),
        );
        let resolved = resolve("test.one", &fx.env).unwrap();
        assert_eq!(resolved.name, "One");
        assert_eq!(resolved.paths.len(), 2);
        assert!(resolved.paths.contains(&fs::canonicalize(&abs).unwrap()));
        assert!(
            resolved
                .paths
                .contains(&fx.env.path[0].canonicalize().unwrap().join("rel-bin"))
        );
        assert!(resolved.notes.is_empty());
    }

    #[test]
    fn quoted_first_word_and_escapes() {
        assert_eq!(
            split_exec(r#""/opt/my app/run" --flag "a b""#),
            vec!["/opt/my app/run", "--flag", "a b"]
        );
        // Field codes are not arguments, `%%` is a literal percent.
        assert_eq!(split_exec(r#"/usr/bin/x %F"#), vec!["/usr/bin/x"]);
        assert_eq!(
            split_exec(r#"/usr/bin/x 100%% %u"#),
            vec!["/usr/bin/x", "100%"]
        );
        // General escapes apply before quoting, so a path with a space has to
        // be quoted; unquoted, the decoded space separates two arguments.
        assert_eq!(split_exec(r#""/opt/my\sapp/run""#), vec!["/opt/my app/run"]);
        assert_eq!(
            split_exec(r#"/opt/my\sapp/run"#),
            vec!["/opt/my", "app/run"]
        );
        assert_eq!(split_exec(r#"/opt/a\\b"#), vec!["/opt/a\\b"]);
        // Inside quotes only the four escapes are special.
        assert_eq!(split_exec(r#"/x "a\"b" "c\d""#), vec!["/x", "a\"b", "c\\d"]);
    }

    #[test]
    fn env_prefix_is_skipped() {
        let fx = fixture("env");
        let one = binary(&fx, "one-bin");
        let two = binary(&fx, "two-bin");
        app(
            &fx,
            "plain",
            &format!(
                "[Desktop Entry]\nName=X\nExec=env A=1 B=two {} --flag %u\n",
                one.display()
            ),
        );
        app(
            &fx,
            "options",
            &format!(
                "[Desktop Entry]\nName=X\nExec=/usr/bin/env -u OLD -i A=1 -- {}\n",
                two.display()
            ),
        );
        assert_eq!(
            resolve("plain", &fx.env).unwrap().paths,
            vec![fs::canonicalize(&one).unwrap()]
        );
        assert_eq!(
            resolve("options", &fx.env).unwrap().paths,
            vec![fs::canonicalize(&two).unwrap()]
        );
        // Not env: untouched. Only env: nothing left to resolve.
        assert_eq!(skip_env(&split_exec("environment A=1")).len(), 2);
        assert!(skip_env(&split_exec("env A=1")).is_empty());
    }

    #[test]
    fn files_read_for_an_account_must_be_its_own() {
        let fx = fixture("owner");
        let home = fx.dir.join("share");
        let file = home.join("applications/mine.desktop");
        fs::write(&file, "[Desktop Entry]\nName=Mine\n").unwrap();
        let mut env = fx.env.clone();
        let me = unsafe { libc::getuid() };
        env.owner = Some((me, home.clone()));
        assert!(env.read(&file).unwrap().contains("Mine"));
        // A link to a file of someone else is refused, whatever its mode.
        let link = home.join("applications/link.desktop");
        std::os::unix::fs::symlink("/etc/passwd", &link).unwrap();
        assert_eq!(
            env.read(&link).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        // The same file under another account's name is refused too.
        env.owner = Some((me + 1, home.clone()));
        assert!(env.read(&file).is_err());
        // Outside the home the owner does not matter, and directories and
        // FIFOs are never read.
        env.owner = Some((me, fx.dir.join("elsewhere")));
        assert!(env.read(&link).is_ok());
        assert!(env.read(&home).is_err());
        let fifo = home.join("applications/pipe.desktop");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0);
        assert!(env.read(&fifo).is_err());
    }

    #[test]
    fn shells_and_own_programs_are_unsuitable() {
        assert!(unsuitable(Path::new("/usr/bin/bash")).is_some());
        assert!(unsuitable(Path::new("/usr/bin/python3.13")).is_some());
        assert!(unsuitable(Path::new("/usr/bin/pkttyagent")).is_some());
        // Other shells and runtimes would lock the session just the same.
        assert!(unsuitable(Path::new("/usr/bin/tcsh")).is_some());
        assert!(unsuitable(Path::new("/usr/bin/busybox")).is_some());
        assert!(unsuitable(Path::new("/usr/bin/lua")).is_some());
        assert!(unsuitable(Path::new("/usr/bin/kwrite")).is_none());
        assert!(unsuitable(Path::new("/opt/firefox-nightly/firefox")).is_none());
    }

    #[test]
    fn several_entries_of_one_app_share_the_path() {
        let fx = fixture("entries");
        let bin = binary(&fx, "editor");
        app(
            &fx,
            "editor",
            &format!("[Desktop Entry]\nName=X\nExec={} %F\n", bin.display()),
        );
        app(
            &fx,
            "editor-wayland",
            &format!(
                "[Desktop Entry]\nName=X\nExec={} --ozone-platform=wayland %F\n",
                bin.display()
            ),
        );
        for id in ["editor", "editor-wayland"] {
            let resolved = resolve(id, &fx.env).unwrap();
            assert_eq!(
                resolved.paths,
                vec![fs::canonicalize(&bin).unwrap()],
                "{id}"
            );
            assert!(resolved.notes.is_empty());
        }
        // A launcher told apart only by its environment is still shared.
        let launcher = binary(&fx, "launcher");
        for id in ["one", "two"] {
            app(
                &fx,
                id,
                &format!(
                    "[Desktop Entry]\nName=X\nExec=env CONF={id} {} -- %u\n",
                    launcher.display()
                ),
            );
        }
        assert!(resolve("one", &fx.env).unwrap().paths.is_empty());
    }

    #[test]
    fn a_user_entry_shadows_the_system_copy_of_the_same_app() {
        let fx = fixture("shadow");
        let bin = binary(&fx, "tool-bin");
        let user = fx.dir.join("user");
        fs::create_dir_all(user.join("applications")).unwrap();
        // The user copy comes first in XDG_DATA_DIRS and is the one a launch
        // goes through. The system file of the same id must not turn this one
        // application into two that share the binary.
        fs::write(
            user.join("applications/tool.desktop"),
            format!("[Desktop Entry]\nName=Tool\nExec={} run\n", bin.display()),
        )
        .unwrap();
        app(
            &fx,
            "tool",
            &format!("[Desktop Entry]\nName=Tool\nExec={} app\n", bin.display()),
        );
        let mut env = fx.env.clone();
        env.data_dirs = vec![user, fx.env.data_dirs[0].clone()];
        let resolved = resolve("tool", &env).unwrap();
        assert_eq!(resolved.paths, vec![fs::canonicalize(&bin).unwrap()]);
        assert!(resolved.notes.is_empty());
    }

    #[test]
    fn actions_of_one_app_do_not_make_its_binary_shared() {
        let fx = fixture("actions");
        let bin = binary(&fx, "multi-bin");
        app(
            &fx,
            "multi",
            &format!(
                "[Desktop Entry]\nName=Multi\nExec={} %U\n[Desktop Action open]\nName=Open\nExec={} open\n[Desktop Action new]\nName=New\nExec={} new-window\n",
                bin.display(),
                bin.display(),
                bin.display()
            ),
        );
        let resolved = resolve("multi", &fx.env).unwrap();
        assert_eq!(resolved.paths, vec![fs::canonicalize(&bin).unwrap()]);
        assert!(resolved.notes.is_empty());
    }

    #[test]
    fn shared_runtime_is_not_marked() {
        let fx = fixture("shared");
        let electron = binary(&fx, "electron");
        for id in ["app.one", "app.two"] {
            app(
                &fx,
                id,
                &format!(
                    "[Desktop Entry]\nName=X\nExec={} /opt/{id}/app.asar\n",
                    electron.display()
                ),
            );
        }
        let resolved = resolve("app.one", &fx.env).unwrap();
        assert!(resolved.paths.is_empty());
        assert_eq!(
            resolved.notes,
            vec![Note::SharedRuntime(fs::canonicalize(&electron).unwrap())]
        );
        // A different binary in another app still resolves.
        let own = binary(&fx, "own-bin");
        app(
            &fx,
            "app.three",
            &format!("[Desktop Entry]\nName=X\nExec={}\n", own.display()),
        );
        assert_eq!(resolve("app.three", &fx.env).unwrap().paths.len(), 1);
    }

    #[test]
    fn duplicate_inode_is_listed_once() {
        let fx = fixture("dedup");
        let bin = binary(&fx, "dup-bin");
        app(
            &fx,
            "dup.one",
            &format!(
                "[Desktop Entry]\nName=A\nExec={}\nExec={}\n",
                bin.display(),
                bin.display()
            ),
        );
        assert_eq!(resolve("dup.one", &fx.env).unwrap().paths.len(), 1);
    }

    #[test]
    fn subdirectory_ids() {
        let fx = fixture("subdir");
        let bin = binary(&fx, "k-bin");
        let dir = fx.env.data_dirs[0].join("applications/kde4");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("kapp.desktop"),
            format!("[Desktop Entry]\nName=K\nExec={}\n", bin.display()),
        )
        .unwrap();
        assert_eq!(resolve("kde4-kapp", &fx.env).unwrap().name, "K");
    }

    #[test]
    fn symlinked_directories_are_not_walked() {
        let fx = fixture("symlink-dir");
        let bin = binary(&fx, "linked-bin");
        let outside = fx.dir.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(
            outside.join("hidden.desktop"),
            format!("[Desktop Entry]\nName=Hidden\nExec={}\n", bin.display()),
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside, fx.env.data_dirs[0].join("applications/linkdir"))
            .unwrap();
        // The subdirectory id would be linkdir-hidden if the link were
        // followed. A user-controlled symlink must not steer the root scan.
        assert!(matches!(
            resolve("linkdir-hidden", &fx.env),
            Err(Error::NotFound)
        ));
        // A symlinked desktop file itself is still read.
        let file = outside.join("linked.desktop");
        fs::write(
            &file,
            format!("[Desktop Entry]\nName=Linked\nExec={}\n", bin.display()),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            &file,
            fx.env.data_dirs[0].join("applications/visible.desktop"),
        )
        .unwrap();
        let installed = installed(&fx.env);
        assert!(
            installed
                .iter()
                .any(|a| a.id == "visible" && a.name == "Linked"),
            "{installed:?}"
        );
        assert!(!installed.iter().any(|a| a.id == "linkdir-hidden"));
    }

    #[test]
    fn flatpak_deployment_entry() {
        let fx = fixture("flatpak");
        let root = fx.dir.join("home/.local/share/flatpak");
        let base = root.join("app/org.example.App/current/active");
        fs::create_dir_all(base.join("files/bin")).unwrap();
        fs::write(
            base.join("metadata"),
            "[Application]\nname=org.example.App\ncommand=example\n",
        )
        .unwrap();
        // Name `flatpak` through the fixture PATH: an absolute /usr/bin path
        // would make the test depend on the host's Flatpak installation.
        binary(&fx, "flatpak");
        fs::rename(fx.dir.join("flatpak"), fx.env.path[0].join("flatpak")).unwrap();
        fs::write(base.join("files/bin/example"), "#!/bin/sh\n").unwrap();
        app(
            &fx,
            "org.example.App",
            "[Desktop Entry]\nName=Example\nExec=flatpak run --branch=stable --arch=x86_64 org.example.App\n",
        );
        let mut env = fx.env.clone();
        env.flatpak_roots = vec![root];
        let resolved = resolve("org.example.App", &env).unwrap();
        // The path keeps `current/active`, so it survives an update.
        assert_eq!(resolved.paths, vec![base.join("files/bin/example")]);
        // Global options before `run` still name the deployment entry.
        app(
            &fx,
            "org.example.App",
            "[Desktop Entry]\nName=Example\nExec=flatpak --user run --branch=stable org.example.App\n",
        );
        assert_eq!(
            resolve("org.example.App", &env).unwrap().paths,
            vec![base.join("files/bin/example")]
        );
        // A metadata command that leaves the deployment is refused.
        fs::write(
            base.join("metadata"),
            "[Application]\ncommand=/usr/bin/sh\n",
        )
        .unwrap();
        assert!(resolve("org.example.App", &env).unwrap().paths.is_empty());
    }

    #[test]
    fn flatpak_id_skips_option_values() {
        let args = |s: &str| split_exec(s);
        assert_eq!(
            flatpak_id(&args(
                "/usr/bin/flatpak run --branch 1.0 --arch x86_64 org.example.App"
            )),
            Some("org.example.App".into())
        );
        assert_eq!(
            flatpak_id(&args("/usr/bin/flatpak run org.example.App")),
            Some("org.example.App".into())
        );
        // Global options before the subcommand are skipped too.
        assert_eq!(
            flatpak_id(&args("/usr/bin/flatpak --user run org.example.App")),
            Some("org.example.App".into())
        );
        assert_eq!(
            flatpak_id(&args(
                "/usr/bin/flatpak --installation test --arch x86_64 run --branch stable org.example.App"
            )),
            Some("org.example.App".into())
        );
        // Not a run invocation: there is no application id to find.
        assert_eq!(
            flatpak_id(&args("/usr/bin/flatpak --user install org.example.App")),
            None
        );
        // Not a reverse-DNS id with three elements, and nothing that can walk
        // a path.
        assert_eq!(flatpak_id(&args("/usr/bin/flatpak run ../etc")), None);
        assert_eq!(flatpak_id(&args("/usr/bin/flatpak run a.b")), None);
    }

    #[test]
    fn lists_installed_apps() {
        let fx = fixture("installed");
        let bin = binary(&fx, "i-bin");
        app(
            &fx,
            "list.two",
            &format!(
                "[Desktop Entry]\nType=Application\nName=Two\nIcon=two\nExec={}\n",
                bin.display()
            ),
        );
        app(
            &fx,
            "list.one",
            &format!(
                "[Desktop Entry]\nType=Application\nName=One\nExec={}\n",
                bin.display()
            ),
        );
        app(
            &fx,
            "list.link",
            "[Desktop Entry]\nType=Link\nName=Site\nURL=https://example.org\n",
        );
        app(
            &fx,
            "list.hidden",
            &format!(
                "[Desktop Entry]\nType=Application\nNoDisplay=true\nName=Hidden\nExec={}\n",
                bin.display()
            ),
        );
        // Hidden=true removes the entry, unlike NoDisplay, which only keeps it
        // out of menus.
        app(
            &fx,
            "list.gone",
            &format!(
                "[Desktop Entry]\nType=Application\nHidden=true\nName=Gone\nExec={}\n",
                bin.display()
            ),
        );
        let ids: Vec<_> = installed(&fx.env)
            .into_iter()
            .map(|a| (a.id, a.name, a.icon))
            .collect();
        assert_eq!(
            ids,
            vec![
                ("list.one".to_string(), "One".to_string(), String::new()),
                ("list.two".to_string(), "Two".to_string(), "two".to_string()),
            ]
        );
        assert!(matches!(
            resolve("list.gone", &fx.env),
            Err(Error::NotFound)
        ));
    }

    #[test]
    fn display_name_falls_back_to_the_id() {
        let fx = fixture("display-name");
        let bin = binary(&fx, "n-bin");
        app(
            &fx,
            "name.one",
            &format!(
                "[Desktop Entry]\nType=Application\nName=One\\sApp\nExec={}\n",
                bin.display()
            ),
        );
        assert_eq!(display_name("name.one", &fx.env), "One App");
        assert_eq!(display_name("name.none", &fx.env), "name.none");
        app(
            &fx,
            "name.hidden",
            &format!(
                "[Desktop Entry]\nType=Application\nHidden=true\nName=H\nExec={}\n",
                bin.display()
            ),
        );
        assert_eq!(display_name("name.hidden", &fx.env), "name.hidden");
    }

    #[test]
    fn own_user_environment_has_usr_share() {
        let uid = unsafe { libc::getuid() };
        let env = Env::for_user(uid).unwrap();
        let home = crate::users::home(uid).unwrap();
        assert!(
            env.data_dirs
                .iter()
                .any(|d| d == &home.join(".local/share"))
        );
        assert!(env.data_dirs.iter().any(|d| d == Path::new("/usr/share")));
    }

    #[test]
    fn localized_names() {
        let fx = fixture("locale");
        let bin = binary(&fx, "l-bin");
        app(
            &fx,
            "loc.one",
            &format!(
                "[Desktop Entry]\nType=Application\nName=Default\nName[zh_CN]=中文\nName[zh]=简中\nExec={}\n",
                bin.display()
            ),
        );
        let mut env = fx.env.clone();
        env.locale = Some("zh_CN.UTF-8".into());
        assert_eq!(display_name("loc.one", &env), "中文");
        env.locale = Some("zh_TW.UTF-8".into());
        assert_eq!(display_name("loc.one", &env), "简中");
        env.locale = Some("de_DE.UTF-8".into());
        assert_eq!(display_name("loc.one", &env), "Default");
        env.locale = None;
        assert_eq!(display_name("loc.one", &env), "Default");
    }

    #[test]
    fn missing_app_and_missing_command() {
        let fx = fixture("missing");
        assert!(matches!(resolve("nope", &fx.env), Err(Error::NotFound)));
        app(
            &fx,
            "bad.one",
            "[Desktop Entry]\nName=B\nExec=does-not-exist --x\n",
        );
        let resolved = resolve("bad.one", &fx.env).unwrap();
        assert!(resolved.paths.is_empty());
        assert_eq!(
            resolved.notes,
            vec![Note::CommandNotFound("does-not-exist".into())]
        );
    }

    #[test]
    fn ignored_groups_and_comments() {
        let fx = fixture("groups");
        let bin = binary(&fx, "grp-bin");
        app(
            &fx,
            "grp.one",
            &format!(
                "# comment\n[Desktop Entry]\nName=N\nNoDisplay=true\n[Extra]\nExec={}\n",
                bin.display()
            ),
        );
        assert!(resolve("grp.one", &fx.env).unwrap().paths.is_empty());
    }
}
