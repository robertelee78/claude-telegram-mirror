//! ANSI color helpers for terminal output.
//!
//! Centralized from duplicates in doctor.rs and setup.rs.

/// Wrap `s` in ANSI green (code 32).
///
/// # Examples
///
/// ```
/// use ctm::colors::green;
///
/// let colored = green("ok");
/// assert!(colored.contains("ok"));
/// assert!(colored.starts_with("\x1b[32m"));
/// assert!(colored.ends_with("\x1b[0m"));
/// ```
pub fn green(s: &str) -> String {
    format!("\x1b[32m{s}\x1b[0m")
}

pub fn yellow(s: &str) -> String {
    format!("\x1b[33m{s}\x1b[0m")
}

pub fn red(s: &str) -> String {
    format!("\x1b[31m{s}\x1b[0m")
}

pub fn cyan(s: &str) -> String {
    format!("\x1b[36m{s}\x1b[0m")
}

pub fn gray(s: &str) -> String {
    format!("\x1b[90m{s}\x1b[0m")
}

pub fn bold(s: &str) -> String {
    format!("\x1b[1m{s}\x1b[0m")
}
