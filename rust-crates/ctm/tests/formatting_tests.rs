//! Integration tests for message formatting and chunking.
//!
//! Extracted from `src/formatting.rs` inline `#[cfg(test)]` module (Story 13.6).

use ctm::formatting::{
    chunk_message, chunk_message_with_options, detect_language, escape_markdown_v2,
    estimate_chunks, format_help, format_session_start, format_status, needs_chunking, short_path,
    strip_ansi, truncate, ChunkOptions, DEFAULT_MAX_LENGTH,
};

// ---- strip_ansi ----

#[test]
fn strip_ansi_removes_codes() {
    assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
    assert_eq!(strip_ansi("no codes"), "no codes");
    assert_eq!(strip_ansi("\x1b[1;32mbold green\x1b[0m"), "bold green");
}

// ---- escapeMarkdownV2 ----

#[test]
fn escape_markdown_v2_escapes_special_chars() {
    assert_eq!(escape_markdown_v2("hello_world"), "hello\\_world");
    assert_eq!(escape_markdown_v2("a*b"), "a\\*b");
    assert_eq!(escape_markdown_v2("1.2"), "1\\.2");
    assert_eq!(escape_markdown_v2("a+b-c"), "a\\+b\\-c");
}

#[test]
fn escape_markdown_v2_preserves_code_blocks() {
    let input = "hello_world ```code_block``` more_text";
    let escaped = escape_markdown_v2(input);
    assert!(escaped.contains("hello\\_world"));
    assert!(escaped.contains("```code_block```"));
    assert!(escaped.contains("more\\_text"));
}

#[test]
fn escape_markdown_v2_preserves_inline_code() {
    let input = "test `code_here` end";
    let escaped = escape_markdown_v2(input);
    assert!(escaped.contains("`code_here`"));
    assert!(escaped.contains("test "));
}

#[test]
fn escape_markdown_v2_not_a_noop() {
    // Verify this is NOT a no-op.
    let input = "special: _*[]()~`>#+-=|{}.!";
    let escaped = escape_markdown_v2(input);
    assert_ne!(input, escaped);
}

// ---- truncate ----

#[test]
fn truncate_short() {
    assert_eq!(truncate("hello", 10), "hello");
}

#[test]
fn truncate_long() {
    assert_eq!(truncate("hello world", 8), "hello...");
}

#[test]
fn truncate_emoji() {
    // 4 emoji chars (each multi-byte in UTF-8)
    let emoji = "\u{1F600}\u{1F601}\u{1F602}\u{1F603}";

    // max_len < 4: return what fits without ellipsis (can't fit content + "...")
    let result = truncate(emoji, 3);
    assert_eq!(result.chars().count(), 3);
    assert!(!result.ends_with("..."));

    // max_len = 4: text is exactly 4 chars, fits without truncation
    let result = truncate(emoji, 4);
    assert_eq!(result, emoji);

    // max_len = 5 with longer input: truncates with ellipsis
    let longer = "\u{1F600}\u{1F601}\u{1F602}\u{1F603}\u{1F604}\u{1F605}";
    let result = truncate(longer, 5);
    assert!(result.ends_with("..."));
    assert_eq!(result.chars().count(), 5);
}

// ---- short_path ----

#[test]
fn short_path_long() {
    assert_eq!(
        short_path("/opt/project/src/utils/config.ts"),
        ".../utils/config.ts"
    );
}

#[test]
fn short_path_short() {
    assert_eq!(short_path("/src/file.ts"), "/src/file.ts");
}

#[test]
fn short_path_three_components() {
    assert_eq!(short_path("/opt/project/file.ts"), ".../project/file.ts");
}

// ---- chunk_message ----

#[test]
fn chunk_short_message() {
    let chunks = chunk_message("hello", 4000);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0], "hello");
}

#[test]
fn chunk_long_message() {
    let text = "a ".repeat(3000); // 6000 chars
    let chunks = chunk_message(&text, 4000);
    assert!(chunks.len() >= 2);
    // All chunks have part headers
    assert!(chunks[0].contains("Part 1/"));
}

#[test]
fn chunk_options_no_headers() {
    let text = "a ".repeat(3000); // 6000 chars
    let opts = ChunkOptions {
        max_length: 4000,
        preserve_code_blocks: true,
        add_part_headers: false,
    };
    let chunks = chunk_message_with_options(&text, &opts);
    assert!(chunks.len() >= 2);
    // No part headers when add_part_headers is false
    assert!(!chunks[0].contains("Part "));
}

#[test]
fn chunk_options_default_matches_chunk_message() {
    let text = "word ".repeat(1500); // 7500 chars
    let chunks_a = chunk_message(&text, 4000);
    let chunks_b = chunk_message_with_options(
        &text,
        &ChunkOptions {
            max_length: 4000,
            ..ChunkOptions::default()
        },
    );
    assert_eq!(chunks_a.len(), chunks_b.len());
}

#[test]
fn chunk_preserves_code_blocks() {
    // Create a message where a code block sits at the split boundary
    let prefix = "x".repeat(3900);
    let code = "```\ncode line\n```";
    let suffix = "more text";
    let text = format!("{prefix}\n{code}\n{suffix}");
    let chunks = chunk_message(&text, 4000);
    // The code block should not be split across chunks
    for chunk in &chunks {
        let backtick_count = chunk.matches("```").count();
        // Either 0 backticks (no code block) or even number (complete blocks)
        assert!(
            backtick_count % 2 == 0,
            "Odd number of ``` in chunk means a split code block"
        );
    }
}

// ---- detect_language ----

#[test]
fn detect_language_rust() {
    assert_eq!(detect_language("fn main() {}"), "rust");
    assert_eq!(detect_language("use std::io;"), "rust");
}

#[test]
fn detect_language_python() {
    assert_eq!(detect_language("def foo():"), "python");
}

#[test]
fn detect_language_typescript() {
    assert_eq!(detect_language("interface User {"), "typescript");
    assert_eq!(detect_language("type Result = string"), "typescript");
    assert_eq!(detect_language("const x: string = 'hello'"), "typescript");
    assert_eq!(detect_language("const y: number = 42"), "typescript");
    assert_eq!(detect_language("const z = [1, 2] as const"), "typescript");
}

#[test]
fn detect_language_javascript_minified_import() {
    // L4.7: import{foo} from 'bar' (minified, no space after import)
    assert_eq!(detect_language("import{foo} from 'bar'"), "javascript");
    assert_eq!(
        detect_language("import{a, b} from \"module\""),
        "javascript"
    );
}

#[test]
fn detect_language_unknown() {
    assert_eq!(detect_language("hello world"), "");
}

// ---- format functions ----

#[test]
fn format_session_start_all_fields() {
    let s = format_session_start("s1", Some("/project"), Some("myhost"));
    assert!(s.contains("s1"));
    assert!(s.contains("myhost"));
    assert!(s.contains("/project"));
}

#[test]
fn format_help_contains_commands() {
    let h = format_help();
    assert!(h.contains("/status"));
    assert!(h.contains("/help"));
    assert!(h.contains("/abort"));
    assert!(h.contains("/toggle"));
}

#[test]
fn format_status_inactive() {
    let s = format_status(false, None, None);
    assert!(s.contains("No active session"));
}

#[test]
fn format_status_active_muted() {
    let s = format_status(true, Some("s1"), Some(true));
    assert!(s.contains("s1"));
    assert!(s.contains("Muted"));
}

// ---- estimate_chunks / needs_chunking ----

#[test]
fn estimate_chunks_short() {
    assert_eq!(estimate_chunks("hello", 4000), 1);
}

#[test]
fn estimate_chunks_long() {
    let text = "x".repeat(10000);
    assert_eq!(estimate_chunks(&text, 4000), 3);
}

#[test]
fn needs_chunking_false() {
    assert!(!needs_chunking("hello", 4000));
}

#[test]
fn needs_chunking_true() {
    let text = "x".repeat(5000);
    assert!(needs_chunking(&text, 4000));
}

// ---- DEFAULT_MAX_LENGTH ----

#[test]
fn default_max_length_value() {
    assert_eq!(DEFAULT_MAX_LENGTH, 4000);
}
