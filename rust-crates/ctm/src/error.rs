/// Application error types for CTM.
///
/// Every error variant has a clear source and context.
/// No panics, no unwrap on untrusted data.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Socket error: {0}")]
    Socket(String),

    #[error("Injection error: {0}")]
    #[allow(dead_code)] // Library API
    Injection(String),

    #[error("Hook error: {0}")]
    #[allow(dead_code)] // Library API
    Hook(String),

    #[error("Database error: {0}")]
    Database(String),

    #[error("Lock error: {0}")]
    Lock(String),

    #[error("Telegram API error: {0}")]
    Telegram(String),

    #[error("Reqwest error: {0}")]
    Reqwest(#[from] reqwest::Error),

    /// Telegram returned HTTP 429 — the bot is globally rate-limited.
    /// During the `retry_after` window NO API calls succeed for this token.
    /// The entire message queue must pause for this duration.
    #[error("Telegram rate limited: retry after {retry_after_secs}s")]
    RateLimited {
        /// Seconds to wait before any API call, as reported by Telegram.
        retry_after_secs: u64,
    },

    /// ADR-024: the forum topic a message was addressed to no longer exists. Not a
    /// reason to drop the message — it is held until a replacement topic exists.
    #[error("Telegram topic {thread_id} no longer exists")]
    TopicGone { thread_id: i64 },

    /// ADR-024: Telegram refused this message's *content* (a 400 other than a missing
    /// topic). Retrying it unchanged cannot succeed.
    #[error("Telegram refused the message: {0}")]
    Rejected(String),
}

pub type Result<T> = std::result::Result<T, AppError>;
