use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Telegram error: {0}")]
    Telegram(String),

    #[error("Injection error: {0}")]
    Injection(String),

    #[error("Lock error: {0}")]
    Lock(String),

    #[error("Session error: {0}")]
    Session(String),

    #[error("Socket error: {0}")]
    Socket(String),
}

pub type Result<T> = std::result::Result<T, AppError>;
