use std::{
    collections::BTreeMap,
    fmt, fs, io,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::NeteaseError;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NeteaseSession {
    pub(super) cookie: String,
    pub(super) user_id: u64,
    pub(super) nickname: String,
}

impl NeteaseSession {
    pub fn nickname(&self) -> &str {
        &self.nickname
    }
}

impl fmt::Debug for NeteaseSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NeteaseSession")
            .field("cookie", &"[REDACTED]")
            .field("user_id", &self.user_id)
            .field("nickname", &self.nickname)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct SessionStore {
    path: PathBuf,
}

impl SessionStore {
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Option<NeteaseSession>, NeteaseError> {
        match fs::read(&self.path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn save(&self, session: &NeteaseSession) -> Result<(), NeteaseError> {
        write_private_json(&self.path, session)
    }

    pub fn clear(&self) -> Result<(), NeteaseError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct RiskStore {
    path: PathBuf,
}

#[derive(Default, Deserialize, Serialize)]
struct LocalPreferences {
    #[serde(default)]
    unofficial_source_accepted: bool,
}

impl RiskStore {
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn accepted(&self) -> Result<bool, NeteaseError> {
        match fs::read(&self.path) {
            Ok(bytes) => {
                Ok(serde_json::from_slice::<LocalPreferences>(&bytes)?.unofficial_source_accepted)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    pub fn accept(&self) -> Result<(), NeteaseError> {
        write_private_json(
            &self.path,
            &LocalPreferences {
                unofficial_source_accepted: true,
            },
        )
    }
}

pub fn local_stores() -> Result<(SessionStore, RiskStore), NeteaseError> {
    let directory = platform_config_dir()?.join("k3");
    Ok((
        SessionStore::at(directory.join("netease-session.json")),
        RiskStore::at(directory.join("netease-preferences.json")),
    ))
}

fn platform_config_dir() -> Result<PathBuf, NeteaseError> {
    #[cfg(windows)]
    if let Some(path) = std::env::var_os("APPDATA").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }

    #[cfg(target_os = "macos")]
    if let Some(path) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path).join("Library/Application Support"));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(path) = std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(path));
        }
        if let Some(path) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(path).join(".config"));
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "cannot determine the K3 configuration directory",
    )
    .into())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoginTicket {
    pub(super) key: String,
    qr_url: String,
}

impl LoginTicket {
    pub(super) fn new(key: impl Into<String>, qr_url: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            qr_url: qr_url.into(),
        }
    }

    pub fn qr_url(&self) -> &str {
        &self.qr_url
    }

    pub fn qr_lines(&self) -> Result<Vec<String>, NeteaseError> {
        use qrcode::render::unicode;

        let code = qrcode::QrCode::new(self.qr_url.as_bytes())
            .map_err(|error| NeteaseError::Protocol(format!("cannot render QR code: {error}")))?;
        Ok(code
            .render::<unicode::Dense1x2>()
            .quiet_zone(true)
            .build()
            .lines()
            .map(str::to_owned)
            .collect())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizedLogin {
    pub(super) cookie: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoginPoll {
    WaitingScan,
    WaitingConfirmation,
    Expired,
    Authorized(AuthorizedLogin),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoginStatus {
    WaitingScan,
    WaitingConfirmation,
    Expired,
    LoggedIn,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountProfile {
    pub(super) user_id: u64,
    pub(super) nickname: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct BrowserCookie {
    pub(super) name: String,
    pub(super) value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ChromeCookieProfile {
    pub(super) id: String,
}

pub(super) trait ChromeCookieSource: Send + Sync {
    fn profiles(&self) -> Result<Vec<ChromeCookieProfile>, NeteaseError>;
    fn cookies(&self, profile: &ChromeCookieProfile) -> Result<Vec<BrowserCookie>, NeteaseError>;
}

pub(super) struct RookieChromeCookieSource;

impl ChromeCookieSource for RookieChromeCookieSource {
    fn profiles(&self) -> Result<Vec<ChromeCookieProfile>, NeteaseError> {
        rookie_cookies::chrome_profiles()
            .map(|descriptors| {
                descriptors
                    .into_iter()
                    .map(|descriptor| ChromeCookieProfile {
                        id: descriptor.profile.profile_id.to_string(),
                    })
                    .collect()
            })
            .map_err(|error| {
                NeteaseError::ChromeLogin(format!("Chrome profiles could not be read: {error}"))
            })
    }

    fn cookies(&self, profile: &ChromeCookieProfile) -> Result<Vec<BrowserCookie>, NeteaseError> {
        let report =
            rookie_cookies::chrome_profile(&profile.id, Some(vec!["music.163.com".to_owned()]))
                .map_err(|error| {
                    NeteaseError::ChromeLogin(format!(
                        "a Chrome profile could not be read: {error}"
                    ))
                })?;
        let mut cookies = Vec::new();
        let mut source_failure = None;

        for source in report
            .profiles
            .into_iter()
            .flat_map(|profile| profile.sources)
            .filter(|source| source.selected)
        {
            let failed = source.status == rookie_cookies::report::SourceStatusCode::failed();
            if source_failure.is_none() {
                source_failure = source
                    .issues
                    .iter()
                    .find_map(|issue| chrome_source_failure_hint(&issue.message))
                    .map(str::to_owned)
                    .or_else(|| {
                        failed.then(|| {
                            source
                                .issues
                                .first()
                                .map_or_else(chrome_source_failure_fallback, |issue| {
                                    chrome_source_failure_message(&issue.message)
                                })
                        })
                    });
            }

            if source.status == rookie_cookies::report::SourceStatusCode::succeeded() {
                cookies.extend(
                    source
                        .cookies
                        .into_iter()
                        .filter(|cookie| matches!(cookie.name.as_str(), "MUSIC_U" | "__csrf"))
                        .map(|cookie| BrowserCookie {
                            name: cookie.name,
                            value: cookie.value,
                        }),
                );
            }
        }

        if !cookies.iter().any(|cookie| cookie.name == "MUSIC_U")
            && let Some(message) = source_failure
        {
            return Err(NeteaseError::ChromeLogin(message));
        }

        Ok(cookies)
    }
}

fn chrome_source_failure_message(message: &str) -> String {
    chrome_source_failure_hint(message).map_or_else(chrome_source_failure_fallback, str::to_owned)
}

fn chrome_source_failure_hint(message: &str) -> Option<&'static str> {
    let normalized = message.to_ascii_lowercase();
    if normalized.contains("share-locked")
        || normalized.contains("sharing violation")
        || normalized.contains("os error 32")
    {
        return Some(
            "Chrome is using its cookie database. Close Chrome completely, then retry Chrome login import.",
        );
    }
    if (normalized.contains("app-bound") || normalized.contains("v20"))
        && (normalized.contains("administrator") || normalized.contains("privilege"))
    {
        return Some(
            "Chrome's App-Bound cookie encryption requires K3 to run as administrator. Restart K3 as administrator, then retry Chrome login import.",
        );
    }

    None
}

fn chrome_source_failure_fallback() -> String {
    "Chrome cookies could not be read. Check that the Chrome profile is accessible, then retry Chrome login import."
        .to_owned()
}

pub(super) fn chrome_cookie_header(cookies: &[BrowserCookie]) -> Option<String> {
    const COOKIE_NAMES: [&str; 2] = ["MUSIC_U", "__csrf"];

    let mut values = BTreeMap::new();
    for cookie in cookies {
        if COOKIE_NAMES.contains(&cookie.name.as_str()) && safe_cookie_value(&cookie.value) {
            values.insert(cookie.name.as_str(), cookie.value.as_str());
        }
    }
    values.get("MUSIC_U")?;
    Some(
        COOKIE_NAMES
            .into_iter()
            .filter_map(|name| values.get(name).map(|value| format!("{name}={value}")))
            .collect::<Vec<_>>()
            .join("; "),
    )
}

fn safe_cookie_value(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte > b' ' && byte != b';' && byte != 0x7f)
}

pub(super) fn write_private_json<T: Serialize>(path: &Path, value: &T) -> Result<(), NeteaseError> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "NetEase data path has no parent",
        )
    })?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(temporary.as_file_mut(), value)?;
    temporary.as_file_mut().write_all(b"\n")?;
    temporary.as_file_mut().sync_all()?;
    temporary
        .persist(path)
        .map(|_| ())
        .map_err(|error| NeteaseError::Io(error.error))
}

#[cfg(test)]
mod chrome_source_failure_tests {
    use super::chrome_source_failure_message;

    #[test]
    fn locked_database_message_is_actionable_and_does_not_disclose_the_profile_path() {
        let message = chrome_source_failure_message(
            r"Windows browser database is share-locked at \\?\C:\Users\Alice\AppData\Local\Google\Chrome\User Data\Default\Network\Cookies (OS error 32); process shutdown is disabled",
        );

        assert_eq!(
            message,
            "Chrome is using its cookie database. Close Chrome completely, then retry Chrome login import."
        );
        assert!(!message.contains("Alice"));
    }

    #[test]
    fn app_bound_encryption_message_explains_the_administrator_requirement() {
        let message = chrome_source_failure_message(
            "Chrome App-Bound cookie decryption requires an administrator process",
        );

        assert_eq!(
            message,
            "Chrome's App-Bound cookie encryption requires K3 to run as administrator. Restart K3 as administrator, then retry Chrome login import."
        );
    }
}
