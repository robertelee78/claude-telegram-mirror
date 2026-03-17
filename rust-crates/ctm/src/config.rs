use crate::error::{AppError, Result};
use serde::{Deserialize, Serialize};
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
    if let Ok(json) = serde_json::to_string_pretty(&status) {
        use std::os::unix::fs::OpenOptionsExt;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
        {
            use std::io::Write;
            let _ = file.write_all(json.as_bytes());
        }
    }
}

/// CTM configuration loaded from env vars > config file > defaults
#[derive(Debug, Clone)]
pub struct Config {
    pub bot_token: String,
    pub chat_id: i64,
    pub enabled: bool,
    pub verbose: bool,
    #[allow(dead_code)] // Library API
    pub approvals: bool,
    pub use_threads: bool,
    pub chunk_size: usize,
    pub rate_limit: u32,
    pub session_timeout: u32,
    #[allow(dead_code)] // Library API
    pub stale_session_timeout_hours: u32,
    pub auto_delete_topics: bool,
    pub topic_delete_delay_minutes: u32,
    pub socket_path: PathBuf,
    pub config_dir: PathBuf,
    /// Resolved path to config.json (may not exist if config was provided via env vars only)
    #[allow(dead_code)] // Library API
    pub config_path: PathBuf,
    /// Whether forum (topics) mode is enabled (default: false)
    #[allow(dead_code)] // Library API
    pub forum_enabled: bool,
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
    #[serde(alias = "socketPath", alias = "socket_path")]
    socket_path: Option<String>,
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

/// Validate a socket path for safety
pub fn validate_socket_path(path: &str) -> bool {
    !path.is_empty() && !path.contains("..") && path.starts_with('/') && path.len() <= 256
}

fn parse_bool(val: &str) -> bool {
    matches!(val.trim().to_lowercase().as_str(), "true" | "1")
}

fn parse_u32(val: &str, default: u32) -> u32 {
    val.trim().parse().unwrap_or(default)
}

fn parse_usize(val: &str, default: usize) -> usize {
    val.trim().parse().unwrap_or(default)
}

fn parse_i64(val: &str, default: i64) -> i64 {
    val.trim().parse().unwrap_or(default)
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
        .map(|v| parse_u32(&v, 1))
        .or(file_config.rate_limit)
        .unwrap_or(1);

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
        .map(|v| parse_u32(&v, 1440))
        .or(file_config.topic_delete_delay_minutes)
        .unwrap_or(1440);

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
        socket_path,
        config_dir,
        config_path,
        forum_enabled: false,
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
        assert!(!validate_socket_path(&format!("/{}", "a".repeat(256))));
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
        assert_eq!(config.rate_limit, 1);
        assert_eq!(config.session_timeout, 30);
        assert_eq!(config.stale_session_timeout_hours, 72);
        assert!(config.auto_delete_topics);
        assert_eq!(config.topic_delete_delay_minutes, 1440);
    }
}
