//! The interface language and the user's saved choice.
//!
//! Shisa ships Simplified Chinese and English. The choice is a per-user,
//! user-writable preference, kept apart from the root-owned policy files:
//! changing it must not need polkit authorization. The settings window writes
//! it and every other user-side program reads it, so the tray, the CLI, the
//! restore window and the authentication detail all agree.

use serde::Deserialize;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// The interface languages Shisa ships with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    ZhCn,
    En,
}

impl Language {
    /// The language for a POSIX locale. Only Chinese locales select Chinese,
    /// so an unset or unknown locale still gets a readable interface.
    pub fn from_locale(locale: Option<&str>) -> Self {
        let Some(locale) = locale else {
            return Self::En;
        };
        let base = locale.split(['.', '@']).next().unwrap_or(locale);
        let head = base.get(..3);
        if base.eq_ignore_ascii_case("zh")
            || head.is_some_and(|h| h.eq_ignore_ascii_case("zh_") || h.eq_ignore_ascii_case("zh-"))
        {
            Self::ZhCn
        } else {
            Self::En
        }
    }

    /// The tag used by the preference file and the settings frontend.
    pub fn tag(self) -> &'static str {
        match self {
            Self::ZhCn => "zh-CN",
            Self::En => "en",
        }
    }

    /// Reads a tag from the preference file or the frontend. An unknown value
    /// is rejected instead of replacing a valid choice.
    pub fn from_tag(tag: &str) -> Option<Self> {
        let tag = tag.trim().to_ascii_lowercase();
        if tag == "zh" || tag.starts_with("zh-") || tag.starts_with("zh_") {
            Some(Self::ZhCn)
        } else if tag == "en" || tag.starts_with("en-") || tag.starts_with("en_") {
            Some(Self::En)
        } else {
            None
        }
    }
}

#[derive(Default, Deserialize)]
struct UiConfig {
    language: Option<String>,
}

/// The preference file in an account's XDG config directory.
pub fn config_path(config_home: &Path) -> PathBuf {
    config_home.join("shisa/ui.toml")
}

/// The preference file is tiny. Root reads it for an account on paths that
/// run without a bounded worker, so a special file or an oversized one must
/// not stall or grow the daemon.
const MAX_UI_CONFIG: u64 = 64 * 1024;

/// The saved choice, or `None` when there is none or the file cannot be read
/// or parsed. The caller then falls back to the locale rather than failing.
pub fn saved(config_home: &Path) -> Option<Language> {
    saved_as(config_home, None)
}

/// The saved choice when root reads it for an account. `owner` is the
/// account's uid: the opened file must then belong to it, so a link to a
/// root-only file is not read on the account's behalf.
pub fn saved_as(config_home: &Path, owner: Option<u32>) -> Option<Language> {
    let text = read_preference(&config_path(config_home), owner).ok()?;
    let config: UiConfig = toml::from_str(&text).ok()?;
    Language::from_tag(config.language.as_deref()?)
}

/// Read the preference as a small regular file without blocking on a FIFO.
/// shisad calls this while it represents the account, so the same limits as
/// the desktop-file reader apply.
fn read_preference(path: &Path, owner: Option<u32>) -> io::Result<String> {
    let text = crate::files::read_small_text(path, MAX_UI_CONFIG, |_, meta| {
        if let Some(uid) = owner
            && meta.uid() != uid
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "not owned by the account",
            ));
        }
        Ok(())
    })?;
    if text.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "contains a NUL byte",
        ));
    }
    Ok(text)
}

/// The language for an account: its saved choice, else its session locale.
pub fn for_account(config_home: &Path, locale: Option<&str>) -> Language {
    for_account_as(config_home, locale, None)
}

/// The language for an account when root reads the preference on its behalf.
pub fn for_account_as(config_home: &Path, locale: Option<&str>, owner: Option<u32>) -> Language {
    saved_as(config_home, owner).unwrap_or_else(|| Language::from_locale(locale))
}

/// The config directory for this process, when HOME is known.
fn config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
}

/// The language for this process.
pub fn current() -> Language {
    let locale = std::env::var("LC_ALL")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("LC_MESSAGES")
                .ok()
                .filter(|value| !value.is_empty())
        })
        .or_else(|| std::env::var("LANG").ok().filter(|value| !value.is_empty()));
    match config_home() {
        Some(config_home) => for_account(&config_home, locale.as_deref()),
        None => Language::from_locale(locale.as_deref()),
    }
}

/// Save this process's choice. Written to a temporary file and renamed, so a
/// crash cannot leave a half-written preference behind.
pub fn save(language: Language) -> io::Result<()> {
    let config_home = config_home()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory"))?;
    save_to(&config_home, language)
}

/// Save a choice in an explicit config directory. Used by `save` and tests.
pub fn save_to(config_home: &Path, language: Language) -> io::Result<()> {
    let path = config_path(config_home);
    let Some(dir) = path.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no config directory",
        ));
    };
    fs::create_dir_all(dir)?;
    let temp = dir.join(".ui.toml.new");
    let text = format!("language = \"{}\"\n", language.tag());
    let written = fs::File::create(&temp)
        .and_then(|mut file| {
            file.write_all(text.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&temp, &path));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

/// Rewrites a known English error from shisad, zbus or polkit for the
/// interface. An unknown message is returned unchanged, so nothing is hidden.
///
/// The daemon keeps its messages in English: they are also part of the D-Bus
/// contract and the debug log. This is the one place a client translates them.
pub fn error_text(text: &str, language: Language) -> String {
    if language == Language::En {
        return text.to_string();
    }
    let hay = text.to_lowercase();
    let hit = ERROR_TEXT.iter().find(|(needle, _)| hay.contains(needle));
    match hit {
        Some((_, translated)) => (*translated).to_string(),
        None => text.to_string(),
    }
}

/// English needles and their Simplified Chinese text, longest first where one
/// message contains another ("not an absolute path" and "protected paths must
/// be absolute" are separate conditions but both name an absolute path).
const ERROR_TEXT: &[(&str, &str)] = &[
    (
        "too many pending authentication requests",
        "还有一个认证没有结束，完成或取消后再试。",
    ),
    (
        "authentication was rejected",
        "认证没有通过，改动没有生效。",
    ),
    ("authentication timed out", "认证超时，改动没有生效。"),
    ("authentication cancelled", "认证取消了，改动没有生效。"),
    (
        "locked before the change was applied",
        "锁定发生在这个改动完成之前，改动没有生效。",
    ),
    (
        "locked before the authorization completed",
        "锁定发生在这个认证完成之前，应用仍然处于锁定状态。",
    ),
    (
        "settings locked",
        "设置已上锁，关闭并重新打开 Shisa 设置后再试。",
    ),
    ("cannot start authentication", "无法启动认证，请稍后再试。"),
    ("polkit unavailable", "认证服务不可用，请稍后再试。"),
    ("grace_seconds must be at most", "宽限期超过了允许的上限。"),
    (
        "prompt_timeout_seconds must be between",
        "认证框等待上限要在 5 到 85 秒之间。",
    ),
    (
        "relock_after_unfocus_seconds must be at most",
        "失焦隐藏时长超过了允许的上限。",
    ),
    ("unsupported option", "不支持这个设置项。"),
    ("does not apply to", "这个设置项不适用于当前目标。"),
    ("too many requests", "请求太多，稍后再试。"),
    ("request dropped", "请求中断了，稍后再试。"),
    (
        "shisa's own programs cannot be protected",
        "Shisa 自己的程序不能受保护，否则认证无法进行。",
    ),
    (
        "protecting a shell, interpreter or system program",
        "这是 shell、解释器或系统程序，保护它会影响整个会话。",
    ),
    (
        "not a regular file",
        "这个路径不是普通文件，要指向可执行文件。",
    ),
    ("cannot read", "读不到这个路径，检查文件是否存在。"),
    ("not an absolute path", "保护路径要写绝对路径。"),
    ("must be absolute", "保护路径要写绝对路径。"),
    (
        "at least one protected path",
        "至少要保留一条保护路径，要放开请取消保护。",
    ),
    (
        "is not protected",
        "这个应用已经不在保护列表里，刷新后再试。",
    ),
    (
        "has no usable protected path",
        "这个应用没有可用的保护路径，检查 desktop 文件或手动添加路径。",
    ),
    (
        "app is no longer protected",
        "这个应用已经不在保护列表里，刷新后再试。",
    ),
    ("desktop file not found", "找不到这个应用的 desktop 文件。"),
    ("invalid app id", "应用 id 不合法。"),
    ("no home directory", "读不到这个账号的家目录。"),
    ("unknown user", "读不到这个账号的资料。"),
    ("serviceunknown", "应用锁没有响应，稍后再试。"),
    ("was not provided", "应用锁没有响应，稍后再试。"),
    ("connection refused", "应用锁没有响应，稍后再试。"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locales_and_tags_map_to_the_two_languages() {
        assert_eq!(Language::from_locale(Some("zh_CN.UTF-8")), Language::ZhCn);
        assert_eq!(Language::from_locale(Some("zh_TW.UTF-8")), Language::ZhCn);
        assert_eq!(Language::from_locale(Some("zh")), Language::ZhCn);
        assert_eq!(Language::from_locale(Some("en_US.UTF-8")), Language::En);
        assert_eq!(Language::from_locale(Some("de_DE.UTF-8")), Language::En);
        // An unset locale is English, not a guess at the user's language.
        assert_eq!(Language::from_locale(None), Language::En);

        assert_eq!(Language::from_tag("zh-CN"), Some(Language::ZhCn));
        assert_eq!(Language::from_tag("zh_TW"), Some(Language::ZhCn));
        assert_eq!(Language::from_tag(" en "), Some(Language::En));
        assert_eq!(Language::from_tag("en-US"), Some(Language::En));
        assert_eq!(Language::from_tag("fr"), None);
        assert_eq!(Language::ZhCn.tag(), "zh-CN");
        assert_eq!(Language::En.tag(), "en");
    }

    #[test]
    fn a_saved_choice_beats_the_locale_and_bad_files_fall_back() {
        let dir = std::env::temp_dir().join(format!("shisa-i18n-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);

        // No file yet: the locale decides.
        assert_eq!(for_account(&dir, Some("zh_CN.UTF-8")), Language::ZhCn);
        assert_eq!(for_account(&dir, Some("en_US.UTF-8")), Language::En);

        save_to(&dir, Language::En).unwrap();
        assert_eq!(saved(&dir), Some(Language::En));
        assert_eq!(for_account(&dir, Some("zh_CN.UTF-8")), Language::En);

        save_to(&dir, Language::ZhCn).unwrap();
        assert_eq!(saved(&dir), Some(Language::ZhCn));

        // A malformed or unknown file is unset, not a reason to fail.
        fs::write(config_path(&dir), "language = \"nonsense\"\n").unwrap();
        assert_eq!(saved(&dir), None);
        assert_eq!(for_account(&dir, Some("en_US.UTF-8")), Language::En);
        fs::write(config_path(&dir), "not toml [").unwrap();
        assert_eq!(saved(&dir), None);
        assert_eq!(for_account(&dir, Some("zh_CN.UTF-8")), Language::ZhCn);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_special_or_oversized_preference_falls_back_to_the_locale() {
        use std::os::unix::ffi::OsStrExt;

        let dir = std::env::temp_dir().join(format!("shisa-i18n-limits-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = config_path(&dir);
        fs::create_dir_all(path.parent().unwrap()).unwrap();

        // A plain read of a FIFO with no writer would never return.
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: mkfifo only reads the NUL-terminated path and creates a
        // node at it.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert_eq!(saved(&dir), None);
        fs::remove_file(&path).unwrap();

        // A file above the cap is refused instead of being read whole.
        let mut big = String::from("language = \"en\"\n");
        big.push_str(&"#".repeat(MAX_UI_CONFIG as usize));
        fs::write(&path, big).unwrap();
        assert_eq!(saved(&dir), None);
        fs::remove_file(&path).unwrap();

        fs::create_dir(&path).unwrap();
        assert_eq!(saved(&dir), None);
        fs::remove_dir(&path).unwrap();

        // Root only reads a file that belongs to the account it represents.
        fs::write(&path, "language = \"en\"\n").unwrap();
        let me = unsafe { libc::getuid() };
        assert_eq!(saved_as(&dir, Some(me)), Some(Language::En));
        assert_eq!(saved_as(&dir, Some(me + 1)), None);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn known_errors_are_translated_only_for_chinese() {
        assert_eq!(
            error_text("authentication was rejected", Language::ZhCn),
            "认证没有通过，改动没有生效。"
        );
        assert_eq!(
            error_text("ServiceUnknown: no owner", Language::ZhCn),
            "应用锁没有响应，稍后再试。"
        );
        // English clients get the daemon's own wording.
        assert_eq!(
            error_text("authentication was rejected", Language::En),
            "authentication was rejected"
        );
        // An unknown message is shown as it came.
        assert_eq!(
            error_text("something new happened", Language::ZhCn),
            "something new happened"
        );
    }
}
