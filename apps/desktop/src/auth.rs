//! OIDC Device Authorization Grant (RFC 8628) against Keycloak, with token persistence.
//! Only used for `wss://` endpoints. Tokens are never printed or logged.
use crate::endpoint::config_string;
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const DEFAULT_ISSUER: &str = "https://user.kigawa.net/realms/develop";
pub const DEFAULT_CLIENT_ID: &str = "velink-desktop";
pub const DEFAULT_SCOPE: &str = "openid offline_access";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// Refresh this long before the access token actually expires.
const REFRESH_MARGIN_SECS: u64 = 30;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthConfig {
    pub issuer: String,
    pub client_id: String,
    pub scope: String,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            issuer: DEFAULT_ISSUER.into(),
            client_id: DEFAULT_CLIENT_ID.into(),
            scope: DEFAULT_SCOPE.into(),
        }
    }
}

impl AuthConfig {
    /// Priority: environment > config file (`issuer`, `client_id`, `scope`) > defaults.
    pub fn resolve(
        config_text: Option<&str>,
        env_issuer: Option<&str>,
        env_client_id: Option<&str>,
    ) -> Self {
        let mut c = Self::default();
        let file = |k: &str| config_text.and_then(|t| config_string(t, k));
        let pick = |env: Option<&str>, k: &str, dst: &mut String| {
            if let Some(v) = env
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
                .or_else(|| file(k))
            {
                if !v.is_empty() {
                    *dst = v;
                }
            }
        };
        pick(env_issuer, "issuer", &mut c.issuer);
        pick(env_client_id, "client_id", &mut c.client_id);
        pick(None, "scope", &mut c.scope);
        c.issuer = c.issuer.trim_end_matches('/').to_owned();
        c
    }

    pub fn from_environment(config_file: Option<&Path>) -> Self {
        let text = config_file.and_then(|p| std::fs::read_to_string(p).ok());
        let var = |k: &str| std::env::var(k).ok();
        Self::resolve(
            text.as_deref(),
            var("VELINK_ISSUER").as_deref(),
            var("VELINK_CLIENT_ID").as_deref(),
        )
    }

    fn device_url(&self) -> String {
        format!("{}/protocol/openid-connect/auth/device", self.issuer)
    }
    fn token_url(&self) -> String {
        format!("{}/protocol/openid-connect/token", self.issuer)
    }
}

#[derive(Debug, Deserialize)]
pub struct DeviceAuthorization {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    #[serde(default = "default_interval")]
    pub interval: u64,
    #[serde(default = "default_expires_in")]
    pub expires_in: u64,
}
fn default_interval() -> u64 {
    5
}
fn default_expires_in() -> u64 {
    600
}

impl DeviceAuthorization {
    pub fn browser_url(&self) -> String {
        match &self.verification_uri_complete {
            Some(u) if !u.is_empty() => u.clone(),
            _ => {
                let sep = if self.verification_uri.contains('?') {
                    '&'
                } else {
                    '?'
                };
                format!("{}{sep}user_code={}", self.verification_uri, self.user_code)
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct OAuthError {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct StoredToken {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Unix seconds at which the access token expires.
    pub expires_at: u64,
}

// Manual Debug so secrets can never leak through `{:?}`.
impl std::fmt::Debug for StoredToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredToken")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl StoredToken {
    fn from_response(r: TokenResponse, previous_refresh: Option<String>, at: u64) -> Self {
        Self {
            access_token: r.access_token,
            refresh_token: r.refresh_token.or(previous_refresh),
            expires_at: at + r.expires_in.unwrap_or(60),
        }
    }
    pub fn is_expired_at(&self, at: u64) -> bool {
        at + REFRESH_MARGIN_SECS >= self.expires_at
    }
}

/// Location of `token.json` (same directory as `config.toml`).
pub fn token_path_from(config: Option<PathBuf>) -> Option<PathBuf> {
    config.map(|p| p.with_file_name("token.json"))
}

pub fn load_token(path: &Path) -> Result<Option<StoredToken>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(serde_json::from_str(&text).ok()), // corrupt file => log in again
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

pub fn save_token(path: &Path, token: &StoredToken) -> Result<(), String> {
    let err = |e: std::io::Error| format!("cannot write {}: {e}", path.display());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(err)?;
    }
    let json = serde_json::to_vec_pretty(token).map_err(|e| e.to_string())?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(err)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(err)?;
    }
    file.write_all(&json).map_err(err)
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .build()
}

/// POST a form; a 4xx body is returned as `Err(OAuthError)` when it parses as one.
fn post_form<T: for<'de> Deserialize<'de>>(
    url: &str,
    form: &[(&str, &str)],
) -> Result<T, PostError> {
    match agent().post(url).send_form(form) {
        Ok(r) => r
            .into_json()
            .map_err(|e| PostError::Other(format!("invalid response: {e}"))),
        Err(ureq::Error::Status(code, r)) => match r.into_json::<OAuthError>() {
            Ok(e) => Err(PostError::OAuth(e)),
            Err(_) => Err(PostError::Other(format!("HTTP {code} from {url}"))),
        },
        Err(e) => Err(PostError::Other(format!("request to {url} failed: {e}"))),
    }
}

enum PostError {
    OAuth(OAuthError),
    Other(String),
}

impl PostError {
    fn message(self) -> String {
        match self {
            PostError::OAuth(e) => match e.error_description {
                Some(d) => format!("{}: {d}", e.error),
                None => e.error,
            },
            PostError::Other(m) => m,
        }
    }
}

/// Outcome of one poll of the token endpoint.
#[derive(Debug, Eq, PartialEq)]
enum Poll {
    Pending,
    SlowDown,
}

fn classify_poll_error(e: OAuthError) -> Result<Poll, String> {
    match e.error.as_str() {
        "authorization_pending" => Ok(Poll::Pending),
        "slow_down" => Ok(Poll::SlowDown),
        "expired_token" => Err("device code expired; run again to log in".into()),
        "access_denied" => Err("login was denied".into()),
        _ => Err(PostError::OAuth(e).message()),
    }
}

pub fn device_login(
    cfg: &AuthConfig,
    out: &mut impl Write,
    open_browser: bool,
) -> Result<StoredToken, String> {
    let auth: DeviceAuthorization = post_form(
        &cfg.device_url(),
        &[("client_id", &cfg.client_id), ("scope", &cfg.scope)],
    )
    .map_err(|e| format!("device authorization failed: {}", e.message()))?;
    let url = auth.browser_url();
    writeln!(out, "Log in to velink in your browser.").ok();
    writeln!(out, "  URL:  {}", auth.verification_uri).ok();
    writeln!(out, "  Code: {}", auth.user_code).ok();
    writeln!(out, "Opening {url}").ok();
    out.flush().ok();
    if open_browser && webbrowser::open(&url).is_err() {
        writeln!(
            out,
            "Could not open a browser; open the URL above manually."
        )
        .ok();
    }
    let mut interval = auth.interval.max(1);
    let deadline = now() + auth.expires_in;
    loop {
        if now() >= deadline {
            return Err("device code expired; run again to log in".into());
        }
        std::thread::sleep(Duration::from_secs(interval));
        let result: Result<TokenResponse, PostError> = post_form(
            &cfg.token_url(),
            &[
                ("grant_type", DEVICE_GRANT),
                ("device_code", &auth.device_code),
                ("client_id", &cfg.client_id),
            ],
        );
        match result {
            Ok(t) => return Ok(StoredToken::from_response(t, None, now())),
            Err(PostError::OAuth(e)) => match classify_poll_error(e)? {
                Poll::Pending => {}
                Poll::SlowDown => interval += 5,
            },
            Err(PostError::Other(m)) => return Err(m),
        }
    }
}

pub fn refresh(cfg: &AuthConfig, token: &StoredToken) -> Result<StoredToken, String> {
    let rt = token.refresh_token.as_deref().ok_or("no refresh token")?;
    let r: TokenResponse = post_form(
        &cfg.token_url(),
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", rt),
            ("client_id", &cfg.client_id),
        ],
    )
    .map_err(PostError::message)?;
    Ok(StoredToken::from_response(
        r,
        token.refresh_token.clone(),
        now(),
    ))
}

/// Return a valid access token: stored → refreshed → new device login.
pub fn obtain_access_token(
    cfg: &AuthConfig,
    path: &Path,
    out: &mut impl Write,
) -> Result<String, String> {
    if let Some(token) = load_token(path)? {
        if !token.is_expired_at(now()) {
            return Ok(token.access_token);
        }
        match refresh(cfg, &token) {
            Ok(t) => {
                save_token(path, &t)?;
                return Ok(t.access_token);
            }
            Err(e) => {
                writeln!(
                    out,
                    "Saved login could not be refreshed ({e}); logging in again."
                )
                .ok();
            }
        }
    }
    let t = device_login(cfg, out, true)?;
    save_token(path, &t)?;
    Ok(t.access_token)
}

/// Discard the stored token (e.g. after the server rejected it).
pub fn forget_token(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_device_response() {
        let a: DeviceAuthorization = serde_json::from_str(
            r#"{"device_code":"d","user_code":"ABCD-EFGH","verification_uri":"https://u/device",
            "verification_uri_complete":"https://u/device?user_code=ABCD-EFGH","expires_in":600,"interval":5}"#,
        )
        .unwrap();
        assert_eq!(a.interval, 5);
        assert_eq!(a.browser_url(), "https://u/device?user_code=ABCD-EFGH");
        let b: DeviceAuthorization = serde_json::from_str(
            r#"{"device_code":"d","user_code":"XY","verification_uri":"https://u/device"}"#,
        )
        .unwrap();
        assert_eq!((b.interval, b.expires_in), (5, 600));
        assert_eq!(b.browser_url(), "https://u/device?user_code=XY");
    }

    #[test]
    fn poll_errors() {
        let e = |s: &str| OAuthError {
            error: s.into(),
            error_description: None,
        };
        assert_eq!(
            classify_poll_error(e("authorization_pending")),
            Ok(Poll::Pending)
        );
        assert_eq!(classify_poll_error(e("slow_down")), Ok(Poll::SlowDown));
        assert!(classify_poll_error(e("expired_token")).is_err());
        assert!(
            classify_poll_error(e("access_denied"))
                .unwrap_err()
                .contains("denied")
        );
    }

    #[test]
    fn token_response_and_expiry() {
        let r: TokenResponse = serde_json::from_str(
            r#"{"access_token":"a","refresh_token":"r","expires_in":300,"token_type":"Bearer"}"#,
        )
        .unwrap();
        let t = StoredToken::from_response(r, None, 1000);
        assert_eq!(t.expires_at, 1300);
        assert!(!t.is_expired_at(1000));
        assert!(t.is_expired_at(1270));
        // refresh keeps the old refresh token when the server omits a new one
        let r: TokenResponse =
            serde_json::from_str(r#"{"access_token":"b","expires_in":60}"#).unwrap();
        assert_eq!(
            StoredToken::from_response(r, Some("old".into()), 0)
                .refresh_token
                .as_deref(),
            Some("old")
        );
        let dbg = format!(
            "{:?}",
            StoredToken {
                access_token: "SECRET-A".into(),
                refresh_token: Some("SECRET-R".into()),
                expires_at: 1
            }
        );
        assert!(!dbg.contains("SECRET"));
    }

    #[test]
    fn token_save_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("velink-auth-{}", std::process::id()));
        let path = dir.join("nested").join("token.json");
        assert_eq!(load_token(&path).unwrap(), None);
        let t = StoredToken {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            expires_at: 42,
        };
        save_token(&path, &t).unwrap();
        assert_eq!(load_token(&path).unwrap(), Some(t.clone()));
        save_token(&path, &t).unwrap(); // overwrite
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::write(&path, "garbage").unwrap();
        assert_eq!(load_token(&path).unwrap(), None);
        forget_token(&path);
        assert!(!path.exists());
    }

    #[test]
    fn config_priority() {
        let d = AuthConfig::resolve(None, None, None);
        assert_eq!(
            (d.issuer.as_str(), d.client_id.as_str()),
            (DEFAULT_ISSUER, DEFAULT_CLIENT_ID)
        );
        let file = "issuer = \"https://f/realms/x/\"\nclient_id = \"fc\"\nscope = \"openid\"\n";
        let c = AuthConfig::resolve(Some(file), None, None);
        assert_eq!(
            (c.issuer.as_str(), c.client_id.as_str(), c.scope.as_str()),
            ("https://f/realms/x", "fc", "openid")
        );
        let c = AuthConfig::resolve(Some(file), Some("https://e/r"), Some("ec"));
        assert_eq!(
            (c.issuer.as_str(), c.client_id.as_str()),
            ("https://e/r", "ec")
        );
        assert_eq!(c.token_url(), "https://e/r/protocol/openid-connect/token");
    }
}
