use crate::error::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::PathBuf;

// ---------------------------------------------------------------- mirror status

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MirrorStatus {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub toggled_at: String,
}

pub fn status_file_path(config_dir: &std::path::Path) -> std::path::PathBuf {
    config_dir.join("status.json")
}

/// Read the current mirroring enabled state from status.json.
/// Returns `true` (default) if the file doesn't exist or can't be parsed.
pub fn read_mirror_status(config_dir: &std::path::Path) -> bool {
    let path = status_file_path(config_dir);
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str::<MirrorStatus>(&content)
            .map(|s| s.enabled)
            .unwrap_or(true),
        Err(_) => true,
    }
}

/// Write the mirroring status file with secure permissions (0o600).
pub fn write_mirror_status(config_dir: &std::path::Path, enabled: bool, pid: Option<u32>) {
    let status = MirrorStatus {
        enabled,
        pid,
        toggled_at: chrono::Utc::now().to_rfc3339(),
    };
    let path = status_file_path(config_dir);
    let json = match serde_json::to_string_pretty(&status) {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to serialize mirror status");
            return;
        }
    };
    use std::os::unix::fs::OpenOptionsExt;
    match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(mut file) => {
            use std::io::Write;
            if let Err(e) = file.write_all(json.as_bytes()) {
                tracing::warn!(path = %path.display(), error = %e, "Failed to write mirror status");
            }
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "Failed to open mirror status file");
        }
    }
}

/// ADR-015: default tool-approval hook wait (seconds). The PreToolUse approval flow
/// (`get_hook_output` → `send_and_wait`) blocks up to this long for a Telegram
/// approve/reject before falling back to the CLI. (AskUserQuestion no longer blocks
/// the hook — it renders natively and mirrors out from `tool_start`, see ADR-015.)
pub const DEFAULT_APPROVAL_WAIT_SECS: u32 = 300;
/// ADR-015: buffer added to `DEFAULT_APPROVAL_WAIT_SECS` for the registered PreToolUse
/// hook timeout, so Claude Code does not cancel the hook before its own wait completes.
pub const APPROVAL_HOOK_TIMEOUT_BUFFER_SECS: u32 = 10;

/// ADR-016: OpenCode host settings. **Enabled by default** (ADR-016 §Default
/// enablement): the daemon provisions the pipe plugin into OpenCode's global config and
/// listens on a local socket, so a bare `opencode` is mirrored with nothing configured.
///
/// `base_url` is only for an OpenCode server ctm should ALSO observe over HTTP — one
/// started with `opencode serve --port N` that the plugin cannot reach (a different
/// user or a container). It needs the explicit port (`--port 0` is random; there is no
/// discovery) and the server password, resolved at connect time as the env var named by
/// `password_env` if set, else `password` from `config.json` (the daemon runs under
/// launchd/systemd without the operator's shell env; `config.json` is mode 0600).
#[derive(Clone, PartialEq, Eq)]
pub struct OpenCodeHostConfig {
    pub enabled: bool,
    pub base_url: Option<String>,
    pub password_env: String,
    pub password: Option<String>,
}

impl OpenCodeHostConfig {
    /// Effective server password: env var first, then config file. `None` means the
    /// observer connects unauthenticated (doctor reports this as a hard failure).
    pub fn resolve_password(&self) -> Option<String> {
        std::env::var(&self.password_env)
            .ok()
            .filter(|p| !p.is_empty())
            .or_else(|| self.password.clone().filter(|p| !p.is_empty()))
    }
}

impl Default for OpenCodeHostConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            base_url: None,
            password_env: "OPENCODE_SERVER_PASSWORD".into(),
            password: None,
        }
    }
}

// S-4 parity: never let the OpenCode password reach a log via {:?}.
impl fmt::Debug for OpenCodeHostConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenCodeHostConfig")
            .field("enabled", &self.enabled)
            .field("base_url", &self.base_url)
            .field("password_env", &self.password_env)
            .field("password", &self.password.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

/// ADR-016: Codex host settings. **Enabled by default**: the daemon keeps Codex's
/// app-server daemon running (`codex app-server daemon start`, idempotent) and a bare
/// `codex` auto-joins it (spike-verified). `binary` overrides the auto-detected native
/// Codex executable (`host::detect::codex_binary`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexHostConfig {
    pub enabled: bool,
    pub socket_path: PathBuf,
    pub binary: Option<PathBuf>,
}

impl Default for CodexHostConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            socket_path: home_dir()
                .join(".codex")
                .join("app-server-control")
                .join("app-server-control.sock"),
            binary: None,
        }
    }
}

/// ADR-016: non-Claude hosts. Both are on unless the operator turns one off
/// (`hosts.<name>.enabled: false`, or `CTM_OPENCODE_ENABLED=0` / `CTM_CODEX_ENABLED=0`).
/// Claude Code needs no entry — it is served by hooks, not an observer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostsConfig {
    pub opencode: OpenCodeHostConfig,
    pub codex: CodexHostConfig,
}

impl HostsConfig {
    /// Enabled non-Claude hosts, in a stable order.
    pub fn enabled(&self) -> Vec<crate::types::HostKind> {
        let mut v = Vec::new();
        if self.opencode.enabled {
            v.push(crate::types::HostKind::OpenCode);
        }
        if self.codex.enabled {
            v.push(crate::types::HostKind::Codex);
        }
        v
    }
}

/// CTM configuration loaded from env vars > config file > defaults
#[derive(Clone)]
pub struct Config {
    pub bot_token: String,
    pub chat_id: i64,
    pub enabled: bool,
    pub verbose: bool,
    #[allow(dead_code)] // Library API
    pub approvals: bool,
    pub use_threads: bool,
    pub chunk_size: usize,
    /// Messages per **minute** to the Telegram group (ADR-024).
    ///
    /// Telegram's own limit, verbatim from the Bot API FAQ: *"In a group, bots are
    /// not able to send more than 20 messages per minute."* Every topic in the forum
    /// is the same group, so this is the whole mirror's budget, shared by every
    /// session. It was read as messages per *second* until 0.2.56 — sixty times the
    /// real ceiling, which is why the bot lived in a permanent 429.
    pub rate_limit: u32,
    pub session_timeout: u32,
    #[allow(dead_code)] // Library API
    pub stale_session_timeout_hours: u32,
    pub auto_delete_topics: bool,
    pub topic_delete_delay_minutes: u32,
    /// ADR-013 E2: Inactivity threshold for topic deletion (default: 720 = 12 hours).
    pub inactivity_delete_threshold_minutes: u32,
    pub socket_path: PathBuf,
    pub config_dir: PathBuf,
    /// Resolved path to config.json (may not exist if config was provided via env vars only)
    #[allow(dead_code)] // Library API
    pub config_path: PathBuf,
    /// Whether forum (topics) mode is enabled (default: false)
    #[allow(dead_code)] // Library API
    pub forum_enabled: bool,
    /// ADR-016: non-Claude hosts to observe.
    pub hosts: HostsConfig,
}

/// S-4: Manual Debug impl that redacts bot_token to prevent accidental log exposure.
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("bot_token", &"[REDACTED]")
            .field("chat_id", &self.chat_id)
            .field("enabled", &self.enabled)
            .field("verbose", &self.verbose)
            .field("approvals", &self.approvals)
            .field("use_threads", &self.use_threads)
            .field("chunk_size", &self.chunk_size)
            .field("rate_limit", &self.rate_limit)
            .field("session_timeout", &self.session_timeout)
            .field(
                "stale_session_timeout_hours",
                &self.stale_session_timeout_hours,
            )
            .field("auto_delete_topics", &self.auto_delete_topics)
            .field(
                "topic_delete_delay_minutes",
                &self.topic_delete_delay_minutes,
            )
            .field(
                "inactivity_delete_threshold_minutes",
                &self.inactivity_delete_threshold_minutes,
            )
            .field("socket_path", &self.socket_path)
            .field("config_dir", &self.config_dir)
            .field("config_path", &self.config_path)
            .field("forum_enabled", &self.forum_enabled)
            .field("hosts", &self.hosts)
            .finish()
    }
}

/// Config file structure (supports both camelCase and snake_case)
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct ConfigFile {
    #[serde(alias = "botToken", alias = "bot_token")]
    bot_token: Option<String>,
    #[serde(alias = "chatId", alias = "chat_id")]
    chat_id: Option<i64>,
    #[serde(alias = "enabled")]
    enabled: Option<bool>,
    #[serde(alias = "verbose")]
    verbose: Option<bool>,
    #[serde(alias = "approvals")]
    approvals: Option<bool>,
    #[serde(alias = "useThreads", alias = "use_threads")]
    use_threads: Option<bool>,
    #[serde(alias = "chunkSize", alias = "chunk_size")]
    chunk_size: Option<usize>,
    #[serde(alias = "rateLimit", alias = "rate_limit")]
    rate_limit: Option<u32>,
    #[serde(alias = "sessionTimeout", alias = "session_timeout")]
    session_timeout: Option<u32>,
    #[serde(
        alias = "staleSessionTimeoutHours",
        alias = "stale_session_timeout_hours"
    )]
    stale_session_timeout_hours: Option<u32>,
    #[serde(alias = "autoDeleteTopics", alias = "auto_delete_topics")]
    auto_delete_topics: Option<bool>,
    #[serde(
        alias = "topicDeleteDelayMinutes",
        alias = "topic_delete_delay_minutes"
    )]
    topic_delete_delay_minutes: Option<u32>,
    #[serde(
        alias = "inactivityDeleteThresholdMinutes",
        alias = "inactivity_delete_threshold_minutes"
    )]
    inactivity_delete_threshold_minutes: Option<u32>,
    #[serde(alias = "socketPath", alias = "socket_path")]
    socket_path: Option<String>,
    /// ADR-016: `"hosts": {"opencode": {"enabled": .., "baseUrl": ..}, "codex": {"enabled": .., "socketPath": .., "binary": ..}}`.
    /// Both hosts are on by default; absent keys keep defaults.
    #[serde(alias = "hosts")]
    hosts: Option<HostsFile>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct HostsFile {
    opencode: Option<OpenCodeHostFile>,
    codex: Option<CodexHostFile>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct OpenCodeHostFile {
    enabled: Option<bool>,
    #[serde(alias = "baseUrl", alias = "base_url")]
    base_url: Option<String>,
    #[serde(alias = "passwordEnv", alias = "password_env")]
    password_env: Option<String>,
    #[serde(alias = "password")]
    password: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct CodexHostFile {
    enabled: Option<bool>,
    #[serde(alias = "socketPath", alias = "socket_path")]
    socket_path: Option<String>,
    binary: Option<String>,
}

/// Fast-path check: is Telegram mirroring enabled based on env vars alone?
///
/// Returns `true` only when all three environment variables are set:
/// `TELEGRAM_MIRROR` is `"true"` or `"1"`, `TELEGRAM_BOT_TOKEN` is non-empty,
/// and `TELEGRAM_CHAT_ID` is non-empty. This avoids loading config files for
/// callers that just need a quick guard.
#[allow(dead_code)] // Library API
pub fn is_mirror_enabled() -> bool {
    std::env::var("TELEGRAM_MIRROR")
        .ok()
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false)
        && std::env::var("TELEGRAM_BOT_TOKEN")
            .ok()
            .filter(|v| !v.is_empty())
            .is_some()
        && std::env::var("TELEGRAM_CHAT_ID")
            .ok()
            .filter(|v| !v.is_empty())
            .is_some()
}

/// Get the user's home directory, falling back to /tmp if unavailable.
///
/// # Examples
///
/// ```
/// use ctm::config::home_dir;
///
/// let dir = home_dir();
/// // The returned path is always absolute.
/// assert!(dir.is_absolute());
/// ```
pub fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// Get the config directory path
pub fn get_config_dir() -> PathBuf {
    home_dir().join(".config").join("claude-telegram-mirror")
}

/// Ensure config directory exists with secure permissions (0o700)
pub fn ensure_config_dir(dir: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if !dir.exists() {
        fs::create_dir_all(dir)?;
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Validate a socket path for safety.
///
/// The length limit of 104 bytes matches the AF_UNIX `sun_path` field size on
/// Linux (108 minus the leading byte and null terminator). Paths longer than
/// this will fail at `bind(2)` with ENAMETOOLONG.
pub fn validate_socket_path(path: &str) -> bool {
    !path.is_empty() && !path.contains("..") && path.starts_with('/') && path.len() <= 104
}

fn parse_bool(val: &str) -> bool {
    matches!(val.trim().to_lowercase().as_str(), "true" | "1")
}

fn parse_u32(val: &str, default: u32) -> u32 {
    val.trim().parse().unwrap_or_else(|_| {
        tracing::warn!(value = val, default, "Invalid u32, using default");
        default
    })
}

fn parse_usize(val: &str, default: usize) -> usize {
    val.trim().parse().unwrap_or_else(|_| {
        tracing::warn!(value = val, default, "Invalid usize, using default");
        default
    })
}

fn parse_i64(val: &str, default: i64) -> i64 {
    val.trim().parse().unwrap_or_else(|_| {
        tracing::warn!(value = val, default, "Invalid i64, using default");
        default
    })
}

/// Load configuration with priority: env vars > config file > defaults
pub fn load_config(require_auth: bool) -> Result<Config> {
    let config_dir = get_config_dir();
    let config_path = config_dir.join("config.json");
    let default_socket = config_dir.join("bridge.sock");

    // Load config file (if exists)
    let file_config = if config_path.exists() {
        match fs::read_to_string(&config_path) {
            Ok(content) => match serde_json::from_str::<ConfigFile>(&content) {
                Ok(cfg) => cfg,
                Err(e) => {
                    tracing::warn!(
                        path = %config_path.display(),
                        error = %e,
                        "Failed to parse config file as JSON, using defaults"
                    );
                    ConfigFile::default()
                }
            },
            Err(e) => {
                tracing::warn!(path = %config_path.display(), error = %e, "Failed to read config file, using defaults");
                ConfigFile::default()
            }
        }
    } else {
        ConfigFile::default()
    };

    // Priority: env > file > default
    let bot_token = std::env::var("TELEGRAM_BOT_TOKEN")
        .ok()
        .or(file_config.bot_token)
        .unwrap_or_default();

    let chat_id = std::env::var("TELEGRAM_CHAT_ID")
        .ok()
        .map(|v| parse_i64(&v, 0))
        .or(file_config.chat_id)
        .unwrap_or(0);

    let enabled = std::env::var("TELEGRAM_MIRROR")
        .ok()
        .map(|v| parse_bool(&v))
        .or(file_config.enabled)
        .unwrap_or(false);

    let verbose = std::env::var("TELEGRAM_MIRROR_VERBOSE")
        .ok()
        .map(|v| parse_bool(&v))
        .or(file_config.verbose)
        .unwrap_or(true);

    let approvals = std::env::var("TELEGRAM_MIRROR_APPROVALS")
        .ok()
        .map(|v| parse_bool(&v))
        .or(file_config.approvals)
        .unwrap_or(true);

    let use_threads = std::env::var("TELEGRAM_USE_THREADS")
        .ok()
        .map(|v| parse_bool(&v))
        .or(file_config.use_threads)
        .unwrap_or(true);

    let chunk_size = std::env::var("TELEGRAM_CHUNK_SIZE")
        .ok()
        .map(|v| parse_usize(&v, 4000))
        .or(file_config.chunk_size)
        .unwrap_or(4000);

    let rate_limit = std::env::var("TELEGRAM_RATE_LIMIT")
        .ok()
        .map(|v| parse_u32(&v, 20))
        .or(file_config.rate_limit)
        .unwrap_or(20);

    let session_timeout = std::env::var("TELEGRAM_SESSION_TIMEOUT")
        .ok()
        .map(|v| parse_u32(&v, 30))
        .or(file_config.session_timeout)
        .unwrap_or(30);

    let stale_session_timeout_hours = std::env::var("TELEGRAM_STALE_SESSION_TIMEOUT_HOURS")
        .ok()
        .map(|v| parse_u32(&v, 72))
        .or(file_config.stale_session_timeout_hours)
        .unwrap_or(72);

    let auto_delete_topics = std::env::var("TELEGRAM_AUTO_DELETE_TOPICS")
        .ok()
        .map(|v| parse_bool(&v))
        .or(file_config.auto_delete_topics)
        .unwrap_or(true);

    let topic_delete_delay_minutes = std::env::var("TELEGRAM_TOPIC_DELETE_DELAY_MINUTES")
        .ok()
        .map(|v| parse_u32(&v, 15))
        .or(file_config.topic_delete_delay_minutes)
        .unwrap_or(15);

    let inactivity_delete_threshold_minutes =
        std::env::var("TELEGRAM_INACTIVITY_DELETE_THRESHOLD_MINUTES")
            .ok()
            .map(|v| parse_u32(&v, 720))
            .or(file_config.inactivity_delete_threshold_minutes)
            .unwrap_or(720);

    // Socket path with validation
    let socket_path = std::env::var("TELEGRAM_BRIDGE_SOCKET")
        .ok()
        .or(file_config.socket_path)
        .and_then(|p| {
            if validate_socket_path(&p) {
                Some(PathBuf::from(p))
            } else {
                tracing::warn!(path = %p, "Invalid socket path, using default");
                None
            }
        })
        .unwrap_or(default_socket);

    if require_auth {
        if bot_token.is_empty() {
            return Err(AppError::Config(
                "TELEGRAM_BOT_TOKEN is required. Create a bot via @BotFather (https://t.me/botfather) and paste the token.".into(),
            ));
        }
        if chat_id == 0 {
            return Err(AppError::Config(
                "TELEGRAM_CHAT_ID is required. Get your chat ID from https://api.telegram.org/bot<YOUR_TOKEN>/getUpdates after sending a message in your group. Supergroup IDs start with -100.".into(),
            ));
        }
    }

    // ADR-016 §Default enablement: both hosts are ON unless turned off. Env vars:
    //   CTM_OPENCODE_ENABLED=0 / CTM_CODEX_ENABLED=0          (opt out)
    //   CTM_OPENCODE_URL=http://127.0.0.1:4096                (also observe an external server)
    //   CTM_CODEX_SOCKET=/path/to/app-server-control.sock     (override the control socket)
    // The config-file form is `"hosts": {"opencode": {...}, "codex": {...}}`.
    let env_flag = |name: &str| -> Option<bool> {
        std::env::var(name).ok().map(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
    };
    let hosts_file = file_config.hosts.unwrap_or_default();
    let opencode = {
        let file = hosts_file.opencode.unwrap_or_default();
        let defaults = OpenCodeHostConfig::default();
        OpenCodeHostConfig {
            enabled: env_flag("CTM_OPENCODE_ENABLED")
                .or(file.enabled)
                .unwrap_or(true),
            base_url: std::env::var("CTM_OPENCODE_URL")
                .ok()
                .or(file.base_url)
                .map(|u| u.trim_end_matches('/').to_string())
                .filter(|u| !u.is_empty()),
            password_env: std::env::var("CTM_OPENCODE_PASSWORD_ENV")
                .ok()
                .or(file.password_env)
                .unwrap_or(defaults.password_env),
            password: file.password,
        }
    };
    let codex = {
        let file = hosts_file.codex.unwrap_or_default();
        let defaults = CodexHostConfig::default();
        CodexHostConfig {
            enabled: env_flag("CTM_CODEX_ENABLED")
                .or(file.enabled)
                .unwrap_or(true),
            socket_path: std::env::var("CTM_CODEX_SOCKET")
                .ok()
                .or(file.socket_path)
                .map(PathBuf::from)
                .unwrap_or(defaults.socket_path),
            binary: std::env::var("CTM_CODEX_BINARY")
                .ok()
                .or(file.binary)
                .filter(|b| !b.is_empty())
                .map(PathBuf::from),
        }
    };
    let hosts = HostsConfig { opencode, codex };

    Ok(Config {
        bot_token,
        chat_id,
        enabled,
        verbose,
        approvals,
        use_threads,
        chunk_size,
        rate_limit,
        session_timeout,
        stale_session_timeout_hours,
        auto_delete_topics,
        topic_delete_delay_minutes,
        inactivity_delete_threshold_minutes,
        socket_path,
        config_dir,
        config_path,
        forum_enabled: false,
        hosts,
    })
}

/// Validate a loaded Config and return (errors, warnings).
///
/// Returns `(errors, warnings)` where errors are fatal misconfigurations
/// and warnings are non-fatal but potentially problematic settings.
pub fn validate_config(config: &Config) -> (Vec<String>, Vec<String>) {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    if config.bot_token.is_empty() {
        errors.push("TELEGRAM_BOT_TOKEN is not set".into());
    }
    if config.chat_id == 0 {
        errors.push("TELEGRAM_CHAT_ID is not set".into());
    }
    if !config.enabled {
        warnings.push("TELEGRAM_MIRROR is not enabled (set to true)".into());
    }
    if config.chunk_size < 1000 || config.chunk_size > 4096 {
        warnings.push(format!(
            "TELEGRAM_CHUNK_SIZE ({}) is outside recommended range (1000-4096)",
            config.chunk_size
        ));
    }

    (errors, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_socket_path() {
        assert!(validate_socket_path("/tmp/bridge.sock"));
        assert!(validate_socket_path("/home/user/.config/ctm/bridge.sock"));
        assert!(!validate_socket_path(""));
        assert!(!validate_socket_path("relative/path.sock"));
        assert!(!validate_socket_path("/tmp/../etc/evil.sock"));
        assert!(!validate_socket_path(&format!("/{}", "a".repeat(104))));
    }

    #[test]
    fn test_parse_bool() {
        assert!(parse_bool("true"));
        assert!(parse_bool("1"));
        assert!(parse_bool("TRUE"));
        assert!(!parse_bool("false"));
        assert!(!parse_bool("0"));
        assert!(!parse_bool("anything"));
    }

    #[test]
    fn test_read_mirror_status_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_mirror_status(tmp.path()));
    }

    #[test]
    fn test_mirror_status_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        write_mirror_status(tmp.path(), false, Some(1234));
        assert!(!read_mirror_status(tmp.path()));
        write_mirror_status(tmp.path(), true, None);
        assert!(read_mirror_status(tmp.path()));
    }

    #[test]
    fn test_read_mirror_status_corrupt() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("status.json"), "not json").unwrap();
        assert!(read_mirror_status(tmp.path()));
    }

    #[test]
    fn test_defaults() {
        // Clear env vars that might interfere
        std::env::remove_var("TELEGRAM_BOT_TOKEN");
        std::env::remove_var("TELEGRAM_CHAT_ID");

        let config = load_config(false).unwrap();
        assert!(config.verbose);
        assert!(config.approvals);
        assert!(config.use_threads);
        assert_eq!(config.chunk_size, 4000);
        assert_eq!(config.rate_limit, 20);
        assert_eq!(config.session_timeout, 30);
        assert_eq!(config.stale_session_timeout_hours, 72);
        assert!(config.auto_delete_topics);
        assert_eq!(config.topic_delete_delay_minutes, 15);
        assert_eq!(config.inactivity_delete_threshold_minutes, 720);
    }
}
