use crate::error::{AppError, Result};
use serde::Deserialize;
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Config {
    pub bot_token: String,
    pub chat_id: i64,
    pub enabled: bool,
    pub verbose: bool,
    pub approvals: bool,
    pub socket_path: PathBuf,
    pub use_threads: bool,
    pub chunk_size: usize,
    pub rate_limit: u32,
    pub session_timeout: u64,
    pub stale_session_timeout_hours: u64,
    pub auto_delete_topics: bool,
    pub topic_delete_delay_minutes: u64,
    pub config_dir: PathBuf,
}

#[derive(Debug, Deserialize, Default)]
struct ConfigFile {
    #[serde(alias = "botToken", alias = "bot_token")]
    bot_token: Option<String>,
    #[serde(alias = "chatId", alias = "chat_id")]
    chat_id: Option<i64>,
    enabled: Option<bool>,
    verbose: Option<bool>,
    approvals: Option<bool>,
    #[serde(alias = "socketPath", alias = "socket_path")]
    socket_path: Option<String>,
    #[serde(alias = "useThreads", alias = "use_threads")]
    use_threads: Option<bool>,
    #[serde(alias = "chunkSize", alias = "chunk_size")]
    chunk_size: Option<usize>,
    #[serde(alias = "rateLimit", alias = "rate_limit")]
    rate_limit: Option<u32>,
    #[serde(alias = "sessionTimeout", alias = "session_timeout")]
    session_timeout: Option<u64>,
    #[serde(
        alias = "staleSessionTimeoutHours",
        alias = "stale_session_timeout_hours"
    )]
    stale_session_timeout_hours: Option<u64>,
    #[serde(alias = "autoDeleteTopics", alias = "auto_delete_topics")]
    auto_delete_topics: Option<bool>,
    #[serde(
        alias = "topicDeleteDelayMinutes",
        alias = "topic_delete_delay_minutes"
    )]
    topic_delete_delay_minutes: Option<u64>,
}

fn config_dir() -> Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| AppError::Config("Cannot determine home directory".to_string()))?;
    Ok(home.join(".config").join("claude-telegram-mirror"))
}

/// Ensure config directory exists with secure permissions (0o700)
pub fn ensure_config_dir(dir: &Path) -> Result<()> {
    if !dir.exists() {
        fs::create_dir_all(dir)?;
    }
    // Security fix #6: config dir chmod 0o700
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Load config file from disk with permission validation
fn load_config_file(config_dir: &Path) -> ConfigFile {
    let config_path = config_dir.join("config.json");
    if !config_path.exists() {
        return ConfigFile::default();
    }

    // Check file permissions before reading
    if let Ok(meta) = fs::metadata(&config_path) {
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            tracing::warn!(
                "Config file has insecure permissions ({:o}), fixing to 0o600",
                mode
            );
            let _ = fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600));
        }
    }

    match fs::read_to_string(&config_path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
            tracing::warn!("Failed to parse config file: {}", e);
            ConfigFile::default()
        }),
        Err(e) => {
            tracing::warn!("Failed to read config file: {}", e);
            ConfigFile::default()
        }
    }
}

fn env_bool(key: &str, default: bool) -> bool {
    match env::var(key) {
        Ok(v) => v == "true" || v == "1",
        Err(_) => default,
    }
}

fn env_u64(key: &str, default: u64) -> u64 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u32(key: &str, default: u32) -> u32 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Load configuration: env vars > config file > defaults
pub fn load_config(require_auth: bool) -> Result<Config> {
    let dir = config_dir()?;
    ensure_config_dir(&dir)?;

    let file = load_config_file(&dir);
    let default_socket = dir.join("bridge.sock");

    let bot_token = env::var("TELEGRAM_BOT_TOKEN")
        .ok()
        .or(file.bot_token)
        .unwrap_or_default();

    let chat_id_str = env::var("TELEGRAM_CHAT_ID")
        .ok()
        .or_else(|| file.chat_id.map(|id| id.to_string()))
        .unwrap_or_default();

    let chat_id: i64 = if chat_id_str.is_empty() {
        0
    } else {
        chat_id_str.parse().map_err(|_| {
            AppError::Config(format!(
                "TELEGRAM_CHAT_ID '{}' is not a valid integer.\n\
                 Chat IDs for supergroups start with -100.",
                chat_id_str
            ))
        })?
    };

    if require_auth {
        if bot_token.is_empty() {
            return Err(AppError::Config(
                "TELEGRAM_BOT_TOKEN is required.\n\
                 Get one from @BotFather on Telegram and set:\n  \
                 export TELEGRAM_BOT_TOKEN=\"your-token-here\""
                    .to_string(),
            ));
        }
        if chat_id == 0 {
            return Err(AppError::Config(
                "TELEGRAM_CHAT_ID is required.\n\
                 Get your chat ID by messaging your bot and visiting:\n  \
                 https://api.telegram.org/bot<TOKEN>/getUpdates\n\
                 Then set:\n  \
                 export TELEGRAM_CHAT_ID=\"your-chat-id\""
                    .to_string(),
            ));
        }
    }

    // MED-02: Validate socket path from env var to prevent path traversal
    let socket_path = env::var("TELEGRAM_BRIDGE_SOCKET")
        .ok()
        .or(file.socket_path)
        .map(|s| {
            if s.contains("..") {
                tracing::warn!(path = %s, "Socket path contains '..', using default");
                return default_socket.clone();
            }
            let p = PathBuf::from(&s);
            if !p.is_absolute() {
                tracing::warn!(path = %s, "Socket path is not absolute, using default");
                return default_socket.clone();
            }
            p
        })
        .unwrap_or(default_socket);

    Ok(Config {
        bot_token,
        chat_id,
        enabled: env_bool("TELEGRAM_MIRROR", file.enabled.unwrap_or(false)),
        verbose: env_bool("TELEGRAM_MIRROR_VERBOSE", file.verbose.unwrap_or(true)),
        approvals: env_bool("TELEGRAM_MIRROR_APPROVALS", file.approvals.unwrap_or(true)),
        socket_path,
        use_threads: env_bool("TELEGRAM_USE_THREADS", file.use_threads.unwrap_or(true)),
        chunk_size: env_usize("TELEGRAM_CHUNK_SIZE", file.chunk_size.unwrap_or(4000)),
        rate_limit: env_u32("TELEGRAM_RATE_LIMIT", file.rate_limit.unwrap_or(1)),
        session_timeout: env_u64(
            "TELEGRAM_SESSION_TIMEOUT",
            file.session_timeout.unwrap_or(30),
        ),
        stale_session_timeout_hours: env_u64(
            "TELEGRAM_STALE_SESSION_TIMEOUT_HOURS",
            file.stale_session_timeout_hours.unwrap_or(72),
        ),
        auto_delete_topics: env_bool(
            "TELEGRAM_AUTO_DELETE_TOPICS",
            file.auto_delete_topics.unwrap_or(true),
        ),
        topic_delete_delay_minutes: env_u64(
            "TELEGRAM_TOPIC_DELETE_DELAY_MINUTES",
            file.topic_delete_delay_minutes.unwrap_or(1440),
        ),
        config_dir: dir,
    })
}

/// Save config to file with secure permissions (0o600)
#[allow(dead_code)]
pub fn save_config(config: &Config) -> Result<()> {
    ensure_config_dir(&config.config_dir)?;

    let config_path = config.config_dir.join("config.json");
    let file_config = serde_json::json!({
        "botToken": config.bot_token,
        "chatId": config.chat_id,
        "enabled": config.enabled,
        "verbose": config.verbose,
        "approvals": config.approvals,
        "useThreads": config.use_threads,
        "autoDeleteTopics": config.auto_delete_topics,
        "topicDeleteDelayMinutes": config.topic_delete_delay_minutes,
    });

    let content = serde_json::to_string_pretty(&file_config)
        .map_err(|e| AppError::Config(format!("Failed to serialize config: {}", e)))?;

    // Security fix #3: Write with 0o600 permissions
    fs::write(&config_path, &content)?;
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))?;

    Ok(())
}

/// Validate configuration
pub fn validate_config(config: &Config) -> (Vec<String>, Vec<String>) {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    if config.bot_token.is_empty() {
        errors.push("TELEGRAM_BOT_TOKEN is not set".to_string());
    }
    if config.chat_id == 0 {
        errors.push("TELEGRAM_CHAT_ID is not set".to_string());
    }
    if !config.enabled {
        warnings.push("TELEGRAM_MIRROR is not enabled".to_string());
    }
    if config.chunk_size < 1000 || config.chunk_size > 4096 {
        warnings.push(format!(
            "Chunk size {} may cause issues (recommended: 1000-4096)",
            config.chunk_size
        ));
    }

    (errors, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_env_bool() {
        assert!(env_bool("NONEXISTENT_VAR_12345", true));
        assert!(!env_bool("NONEXISTENT_VAR_12345", false));
    }

    #[test]
    fn test_config_file_default() {
        let file = ConfigFile::default();
        assert!(file.bot_token.is_none());
        assert!(file.chat_id.is_none());
    }
}
