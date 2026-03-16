#[allow(dead_code)]
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
    Injection(String),

    #[error("Hook error: {0}")]
    Hook(String),
}

pub type Result<T> = std::result::Result<T, AppError>;
