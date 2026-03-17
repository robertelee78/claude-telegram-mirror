//! Telegram Bot API client using reqwest.
//!
//! Thin wrapper around the Bot API with:
//! - Rate limiting via `governor`
//! - Message queue with retry + exponential backoff (3 retries, 1s/2s/4s)
//! - TOPIC_CLOSED recovery (reopen + retry)
//! - Entity parse error fallback (strip formatting, retry as plain text)
//! - Bot token scrubbing on all error messages
//!
//! Ported from `telegram.ts`.

mod client;
mod queue;
mod types;

// Re-export public types
pub use client::TelegramBot;
#[allow(unused_imports)]
pub use types::{
    BotUser, CallbackQuery, InlineButton, PhotoSize, SendOptions, TgChat, TgDocument, TgMessage,
    TgUser, Update,
};

use crate::config::Config;
use crate::error::{AppError, Result};
use crate::formatting::chunk_message;
use governor::{Quota, RateLimiter};
use regex::Regex;
use reqwest::Client;
use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use tokio::sync::Mutex;

// Re-export internal types for sub-modules
use types::{ForumTopicResult, MessagePriority, QueuedMessage, TgFile, TgResponse, TOPIC_COLORS};
// Make PriorityMessageQueue visible to client.rs (via `use super::*`)
use queue::PriorityMessageQueue;

static BOT_TOKEN_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"bot\d+:[A-Za-z0-9_-]+/").unwrap());

// ---------------------------------------------------------------- helpers

/// Current epoch time in milliseconds.
fn epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Create the standard approval keyboard buttons for an approval request.
///
/// Returns a list of rows, where each row is a list of `(text, callback_data)` tuples.
/// The layout matches the TypeScript implementation: Approve + Reject on one row,
/// Abort Session on its own row.
pub fn create_approval_keyboard(approval_id: &str) -> Vec<Vec<InlineButton>> {
    vec![
        vec![
            InlineButton {
                text: "\u{2705} Approve".into(),
                callback_data: format!("approve:{}", approval_id),
            },
            InlineButton {
                text: "\u{274C} Reject".into(),
                callback_data: format!("reject:{}", approval_id),
            },
        ],
        vec![InlineButton {
            text: "\u{1F6D1} Abort Session".into(),
            callback_data: format!("abort:{}", approval_id),
        }],
    ]
}

/// Sanitize a file path to a safe upload filename (basename only).
fn sanitize_upload_filename(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .replace(['/', '\\'], "_")
}

/// Build an inline keyboard JSON structure from our button list.
/// Layout: two buttons per row, matching TypeScript's `if (idx + 1) % 2 === 0 keyboard.row()`.
fn build_inline_keyboard(buttons: &[InlineButton]) -> serde_json::Value {
    let mut rows: Vec<serde_json::Value> = Vec::new();
    let mut current_row: Vec<serde_json::Value> = Vec::new();

    for (idx, btn) in buttons.iter().enumerate() {
        current_row.push(serde_json::json!({
            "text": btn.text,
            "callback_data": btn.callback_data,
        }));
        // Start a new row after every 2nd button
        if (idx + 1) % 2 == 0 {
            rows.push(serde_json::Value::Array(current_row));
            current_row = Vec::new();
        }
    }
    // Flush any remaining button(s) in the last row
    if !current_row.is_empty() {
        rows.push(serde_json::Value::Array(current_row));
    }

    serde_json::json!({"inline_keyboard": rows})
}

/// Strip markdown formatting for plain text fallback.
fn strip_markdown(text: &str) -> String {
    text.replace("**", "")
        .replace('*', "")
        .replace('`', "'")
        .replace('_', "")
}

/// Scrub bot token from log messages to prevent leaking.
///
/// Applies a regex `bot\d+:[A-Za-z0-9_-]+/` globally, matching **any** Telegram
/// API URL token regardless of whether the runtime token is known. This is an
/// intentional improvement over the TypeScript implementation which only replaced
/// the literal configured token value. The regex approach catches tokens from
/// any source (e.g. error messages from third-party libraries that interpolate
/// different tokens) and does not require the runtime token to be available.
pub fn scrub_bot_token(text: &str) -> String {
    BOT_TOKEN_REGEX
        .replace_all(text, "bot[REDACTED]/")
        .into_owned()
}

// ===================================================================== tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_upload_filename() {
        use std::path::Path;
        assert_eq!(
            sanitize_upload_filename(Path::new("/tmp/photo.png")),
            "photo.png"
        );
        assert_eq!(
            sanitize_upload_filename(Path::new("/a/b/c/doc.pdf")),
            "doc.pdf"
        );
        assert_eq!(sanitize_upload_filename(Path::new("/tmp/file")), "file");
        assert_eq!(sanitize_upload_filename(Path::new("/")), "file"); // root has no filename
    }

    #[test]
    fn test_strip_markdown() {
        assert_eq!(strip_markdown("**bold**"), "bold");
        assert_eq!(strip_markdown("*italic*"), "italic");
        assert_eq!(strip_markdown("`code`"), "'code'");
        assert_eq!(strip_markdown("_under_"), "under");
    }

    #[test]
    fn test_scrub_bot_token() {
        let token = "123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11";
        let msg = format!("Error: bot{}/sendMessage failed", token);
        let scrubbed = scrub_bot_token(&msg);
        assert!(!scrubbed.contains(token));
        assert!(scrubbed.contains("[REDACTED]"));
    }

    #[test]
    fn test_scrub_bot_token_regex_no_literal_needed() {
        // The regex approach scrubs any token pattern without needing the runtime token.
        let msg = "POST https://api.telegram.org/bot987654321:XyZ-abc_123DEF456/sendMessage";
        let scrubbed = scrub_bot_token(msg);
        assert!(!scrubbed.contains("987654321:XyZ-abc_123DEF456"));
        assert!(scrubbed.contains("bot[REDACTED]/sendMessage"));
    }

    #[test]
    fn test_scrub_bot_token_no_token_in_text() {
        let msg = "some error message without any token";
        assert_eq!(scrub_bot_token(msg), msg);
    }

    #[test]
    fn test_scrub_bot_token_multiple_occurrences() {
        let msg = "bot111:AAA_bbb-ccc/getMe and bot222:DDD_eee-fff/sendMessage";
        let scrubbed = scrub_bot_token(msg);
        assert!(!scrubbed.contains("111:AAA_bbb-ccc"));
        assert!(!scrubbed.contains("222:DDD_eee-fff"));
        assert_eq!(
            scrubbed,
            "bot[REDACTED]/getMe and bot[REDACTED]/sendMessage"
        );
    }

    #[test]
    fn test_build_inline_keyboard_two_per_row() {
        let buttons = vec![
            InlineButton {
                text: "Approve".into(),
                callback_data: "approve:1".into(),
            },
            InlineButton {
                text: "Reject".into(),
                callback_data: "reject:1".into(),
            },
        ];
        let kb = build_inline_keyboard(&buttons);
        let rows = kb["inline_keyboard"].as_array().unwrap();
        // Two buttons should be in a single row
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_build_inline_keyboard_three_buttons() {
        let buttons = vec![
            InlineButton {
                text: "A".into(),
                callback_data: "a".into(),
            },
            InlineButton {
                text: "B".into(),
                callback_data: "b".into(),
            },
            InlineButton {
                text: "C".into(),
                callback_data: "c".into(),
            },
        ];
        let kb = build_inline_keyboard(&buttons);
        let rows = kb["inline_keyboard"].as_array().unwrap();
        // First row has 2 buttons, second row has 1
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].as_array().unwrap().len(), 2);
        assert_eq!(rows[1].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_topic_colors() {
        assert_eq!(TOPIC_COLORS.len(), 6);
        // Blue first
        assert_eq!(TOPIC_COLORS[0], 0x6FB9F0);
    }

    #[test]
    fn test_create_approval_keyboard_layout() {
        let keyboard = create_approval_keyboard("test-approval-123");
        // Two rows: [Approve, Reject] and [Abort Session]
        assert_eq!(keyboard.len(), 2);
        assert_eq!(keyboard[0].len(), 2);
        assert_eq!(keyboard[1].len(), 1);

        assert!(keyboard[0][0].callback_data.starts_with("approve:"));
        assert!(keyboard[0][1].callback_data.starts_with("reject:"));
        assert!(keyboard[1][0].callback_data.starts_with("abort:"));

        // Verify approval_id is embedded
        assert_eq!(keyboard[0][0].callback_data, "approve:test-approval-123");
        assert_eq!(keyboard[0][1].callback_data, "reject:test-approval-123");
        assert_eq!(keyboard[1][0].callback_data, "abort:test-approval-123");
    }

    #[test]
    fn test_sanitize_upload_filename_with_slashes_in_name() {
        use std::path::Path;
        // A filename that itself contains slash replacements
        assert_eq!(
            sanitize_upload_filename(Path::new("/tmp/some_file.txt")),
            "some_file.txt"
        );
    }

    #[test]
    fn test_strip_markdown_combined() {
        assert_eq!(
            strip_markdown("**Hello** _world_ `code` *italic*"),
            "Hello world 'code' italic"
        );
    }

    #[test]
    fn test_strip_markdown_empty() {
        assert_eq!(strip_markdown(""), "");
    }

    #[test]
    fn test_build_inline_keyboard_empty() {
        let buttons: Vec<InlineButton> = vec![];
        let kb = build_inline_keyboard(&buttons);
        let rows = kb["inline_keyboard"].as_array().unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn test_build_inline_keyboard_single_button() {
        let buttons = vec![InlineButton {
            text: "Only".into(),
            callback_data: "only".into(),
        }];
        let kb = build_inline_keyboard(&buttons);
        let rows = kb["inline_keyboard"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_epoch_millis_positive() {
        let ms = epoch_millis();
        // Should be after 2024-01-01 (1704067200000)
        assert!(ms > 1_704_067_200_000);
    }
}
