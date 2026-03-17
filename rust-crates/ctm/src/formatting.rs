//! Message formatting and chunking for Telegram display.
//!
//! Ported from `formatting.ts` and `chunker.ts`.

use regex::Regex;
use std::sync::LazyLock;

/// Default maximum message length for Telegram (characters).
/// Telegram's hard limit is 4096 but we use 4000 to leave room for part headers.
#[allow(dead_code)] // Library API
pub const DEFAULT_MAX_LENGTH: usize = 4000;

// ------------------------------------------------------------------- ANSI

static ANSI_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]").unwrap());

/// Strip ANSI escape codes from text.
pub fn strip_ansi(text: &str) -> String {
    ANSI_RE.replace_all(text, "").into_owned()
}

// ------------------------------------------------------------- MarkdownV2

/// Characters that must be escaped outside code blocks.
#[allow(dead_code)] // Used by escape_markdown_v2 (Library API)
const MD_SPECIAL: &[char] = &[
    '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!', '\\',
];

/// Escape MarkdownV2 special characters **outside** code blocks.
///
/// Code blocks (triple-backtick and single-backtick) are left untouched.
#[allow(dead_code)] // Library API
pub fn escape_markdown_v2(text: &str) -> String {
    // Split on code blocks: ```...``` or `...`
    // Regex captures code spans so they appear at odd indices.
    static CODE_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(```[\s\S]*?```|`[^`]+`)").unwrap());

    let mut result = String::with_capacity(text.len() + text.len() / 4);
    let mut last_end = 0;

    for m in CODE_RE.find_iter(text) {
        // Text before this code span — escape it.
        let before = &text[last_end..m.start()];
        escape_plain(before, &mut result);
        // Code span — pass through untouched.
        result.push_str(m.as_str());
        last_end = m.end();
    }
    // Trailing plain text.
    let tail = &text[last_end..];
    escape_plain(tail, &mut result);

    result
}

#[allow(dead_code)] // Used by escape_markdown_v2
fn escape_plain(text: &str, out: &mut String) {
    for ch in text.chars() {
        if MD_SPECIAL.contains(&ch) {
            out.push('\\');
        }
        out.push(ch);
    }
}

// -------------------------------------------------------- format helpers

pub fn format_agent_response(content: &str) -> String {
    let cleaned = strip_ansi(content);
    format!("\u{1F916} *Claude:*\n\n{cleaned}")
}

pub fn format_tool_execution(
    tool: &str,
    input: Option<&str>,
    output: Option<&str>,
    verbose: bool,
) -> String {
    let mut msg = format!("\u{1F527} *Tool: {tool}*\n");

    if verbose {
        if let Some(inp) = input {
            let truncated = truncate(inp, 500);
            msg.push_str(&format!("\n\u{1F4E5} Input:\n```\n{truncated}\n```\n"));
        }
    }

    if let Some(out) = output {
        let cleaned = strip_ansi(&truncate(out, 1000));
        msg.push_str(&format!("\n\u{1F4E4} Output:\n```\n{cleaned}\n```"));
    }

    msg
}

pub fn format_approval_request(prompt: &str) -> String {
    let cleaned = strip_ansi(prompt);
    format!("\u{26A0}\u{FE0F} *Approval Required*\n\n{cleaned}\n\nPlease respond:")
}

pub fn format_error(error: &str) -> String {
    let cleaned = strip_ansi(error);
    format!("\u{274C} *Error:*\n\n```\n{cleaned}\n```")
}

pub fn format_session_start(
    session_id: &str,
    project_dir: Option<&str>,
    hostname: Option<&str>,
) -> String {
    let mut msg = format!("\u{1F680} *Session Started*\n\nSession ID: `{session_id}`");
    if let Some(h) = hostname {
        msg.push_str(&format!("\nHost: `{h}`"));
    }
    if let Some(p) = project_dir {
        msg.push_str(&format!("\nProject: `{p}`"));
    }
    msg
}

pub fn format_session_end(session_id: &str, duration_ms: Option<u64>) -> String {
    let mut msg = format!("\u{1F44B} *Session Ended*\n\nSession ID: `{session_id}`");
    if let Some(d) = duration_ms {
        let minutes = d / 60_000;
        let seconds = (d % 60_000) / 1000;
        msg.push_str(&format!("\nDuration: {minutes}m {seconds}s"));
    }
    msg
}

#[allow(dead_code)] // Library API
pub fn format_status(is_active: bool, session_id: Option<&str>, muted: Option<bool>) -> String {
    if !is_active {
        return "\u{1F4CA} *Status*\n\nNo active session attached.".to_string();
    }
    let sid = session_id.unwrap_or("unknown");
    let notif = if muted == Some(true) {
        "\u{1F507} Muted"
    } else {
        "\u{1F514} Active"
    };
    format!("\u{1F4CA} *Status*\n\nSession: `{sid}`\nNotifications: {notif}")
}

pub fn format_help() -> String {
    "\u{1F4DA} *Claude Code Mirror - Commands*

/status - Show current session status
/sessions - List active sessions
/attach <id> - Attach to a session
/detach - Detach from current session
/mute - Mute notifications
/unmute - Resume notifications
/toggle - Toggle Telegram mirroring on/off
/abort - Abort current session
/help - Show this message

*Inline Responses:*
Simply reply with text to send input to the attached session.

*Approval Buttons:*
When Claude requests permission, tap:
\u{2705} Approve - Allow the action
\u{274C} Reject - Deny the action
\u{1F6D1} Abort - End the session"
        .to_string()
}

// --------------------------------------------------------- tool details

pub fn format_tool_details(tool: &str, input: &serde_json::Value) -> String {
    match tool {
        "Edit" => {
            let file = short_path(json_str(input, "file_path"));
            let old = json_str(input, "old_string");
            let new = json_str(input, "new_string");

            let mut msg = format!("\u{270F}\u{FE0F} *Edit*\n\u{1F4C4} `{file}`\n\n");
            if !old.is_empty() {
                msg.push_str(&format!(
                    "\u{2796} *Remove:*\n```\n{}\n```\n\n",
                    truncate(old, 800)
                ));
            }
            if !new.is_empty() {
                msg.push_str(&format!(
                    "\u{2795} *Add:*\n```\n{}\n```",
                    truncate(new, 800)
                ));
            }
            msg
        }
        "Write" => {
            let file = short_path(json_str(input, "file_path"));
            let content = json_str(input, "content");
            let lines = content.lines().count();
            format!(
                "\u{1F4DD} *Write*\n\u{1F4C4} `{file}`\n\u{1F4CF} {lines} lines\n\n```\n{}\n```",
                truncate(content, 1500)
            )
        }
        "Read" => {
            let file = short_path(json_str(input, "file_path"));
            let mut msg = format!("\u{1F441} *Read*\n\u{1F4C4} `{file}`");
            if let Some(offset) = input.get("offset").and_then(|v| v.as_u64()) {
                msg.push_str(&format!("\n\u{1F4CD} Line {offset}"));
            }
            if let Some(limit) = input.get("limit").and_then(|v| v.as_u64()) {
                msg.push_str(&format!(" (+{limit} lines)"));
            }
            msg
        }
        "Bash" => {
            let cmd = json_str(input, "command");
            let mut msg = format!("\u{1F4BB} *Bash*\n\n```bash\n{}\n```", truncate(cmd, 1500));
            if let Some(t) = input.get("timeout").and_then(|v| v.as_u64()) {
                msg.push_str(&format!("\n\u{23F1} Timeout: {t}ms"));
            }
            msg
        }
        "Grep" => {
            let pattern = json_str(input, "pattern");
            let path = if let Some(p) = input.get("path").and_then(|v| v.as_str()) {
                short_path(p)
            } else {
                "cwd".to_string()
            };
            let mut msg = format!(
                "\u{1F50D} *Grep*\n\u{1F3AF} Pattern: `{}`\n\u{1F4C2} Path: `{path}`",
                truncate(pattern, 100)
            );
            if let Some(g) = input.get("glob").and_then(|v| v.as_str()) {
                msg.push_str(&format!("\n\u{1F4CB} Glob: `{g}`"));
            }
            msg
        }
        "Glob" => {
            let pattern = json_str(input, "pattern");
            let path = if let Some(p) = input.get("path").and_then(|v| v.as_str()) {
                short_path(p)
            } else {
                "cwd".to_string()
            };
            format!("\u{1F4C2} *Glob*\n\u{1F3AF} Pattern: `{pattern}`\n\u{1F4C2} Path: `{path}`")
        }
        "Task" => {
            let desc = json_str(input, "description");
            let prompt = json_str(input, "prompt");
            let mut msg = format!("\u{1F916} *Task*\n\u{1F4CB} {desc}");
            if !prompt.is_empty() {
                msg.push_str(&format!("\n\n```\n{}\n```", truncate(prompt, 1000)));
            }
            msg
        }
        "WebFetch" => {
            let url = json_str(input, "url");
            let prompt = json_str(input, "prompt");
            format!(
                "\u{1F310} *WebFetch*\n\u{1F517} `{}`\n\u{1F4DD} {}",
                truncate(url, 100),
                truncate(prompt, 200)
            )
        }
        "WebSearch" => {
            let query = json_str(input, "query");
            format!("\u{1F50E} *WebSearch*\n\u{1F4DD} \"{query}\"")
        }
        "TodoWrite" => {
            let todos = input
                .get("todos")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if todos.is_empty() {
                return "\u{1F4CB} *TodoWrite*".to_string();
            }
            let mut msg = format!("\u{1F4CB} *TodoWrite* ({} items)\n\n", todos.len());
            for todo in todos.iter().take(10) {
                let status = todo
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("pending");
                let content = todo.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let emoji = match status {
                    "in_progress" => "\u{1F504}",
                    "completed" => "\u{2705}",
                    _ => "\u{2B1C}",
                };
                msg.push_str(&format!("{emoji} {}\n", truncate(content, 60)));
            }
            if todos.len() > 10 {
                msg.push_str(&format!("... +{} more", todos.len() - 10));
            }
            msg
        }
        _ => {
            let json_str = serde_json::to_string_pretty(input).unwrap_or_default();
            format!(
                "\u{1F527} *{tool}*\n\n```json\n{}\n```",
                truncate(&json_str, 2000)
            )
        }
    }
}

fn json_str<'a>(v: &'a serde_json::Value, key: &str) -> &'a str {
    v.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

// --------------------------------------------------- language detection

/// Heuristic code-language detection.
#[allow(dead_code)] // Library API
pub fn detect_language(content: &str) -> &'static str {
    let trimmed = content.trim();
    type LangCheck = (&'static str, fn(&str) -> bool);
    // L4.7: TypeScript is checked before JavaScript because TS-specific
    // patterns (type annotations, interface/type keywords) should match first.
    // JavaScript detection now also handles minified `import{` without space.
    let patterns: &[LangCheck] = &[
        ("typescript", |t: &str| {
            t.contains(": string")
                || t.contains(": number")
                || t.contains(": boolean")
                || t.starts_with("interface ")
                || t.starts_with("type ")
                || t.contains("as const")
        }),
        ("javascript", |t: &str| {
            t.starts_with("#!/usr/bin/env node")
                || ((t.starts_with("import ") || t.starts_with("import{"))
                    && (t.contains("from '") || t.contains("from \"")))
                || (t.starts_with("const ") && t.contains(" = require("))
        }),
        ("python", |t: &str| {
            t.starts_with("#!/usr/bin/env python")
                || t.starts_with("import ")
                || (t.starts_with("from ") && t.contains(" import "))
                || t.starts_with("def ")
        }),
        ("go", |t: &str| {
            t.starts_with("package ") || t.starts_with("import \"") || t.starts_with("func ")
        }),
        ("rust", |t: &str| {
            t.starts_with("use ")
                || t.starts_with("fn ")
                || t.starts_with("let mut ")
                || t.starts_with("impl ")
        }),
        ("cpp", |t: &str| {
            t.starts_with("#include ") || t.starts_with("int main(") || t.starts_with("void ")
        }),
        ("bash", |t: &str| {
            t.starts_with("$ ") || t.starts_with("#!") || (t.starts_with('#') && t.contains("bash"))
        }),
        ("json", |t: &str| {
            (t.starts_with('{') && t.ends_with('}')) || (t.starts_with('[') && t.ends_with(']'))
        }),
        ("xml", |t: &str| {
            t.starts_with("<?xml") || t.starts_with("<!DOCTYPE") || t.starts_with("<html")
        }),
    ];

    for &(lang, check) in patterns {
        if check(trimmed) {
            return lang;
        }
    }
    ""
}

#[allow(dead_code)] // Library API
pub fn wrap_in_code_block(content: &str, language: Option<&str>) -> String {
    let lang = language.unwrap_or_else(|| detect_language(content));
    format!("```{lang}\n{content}\n```")
}

// ------------------------------------------------------------- truncate

/// UTF-8 safe truncation with ellipsis.
pub fn truncate(text: &str, max_len: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_len {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_len.saturating_sub(3)).collect();
    format!("{truncated}...")
}

/// Estimate the number of chunks needed to fit `text` within `max_length`.
#[allow(dead_code)] // Library API
pub fn estimate_chunks(text: &str, max_length: usize) -> usize {
    if text.len() <= max_length {
        1
    } else {
        text.len().div_ceil(max_length)
    }
}

/// Returns `true` if `text` exceeds `max_length` and will need chunking.
#[allow(dead_code)] // Library API
pub fn needs_chunking(text: &str, max_length: usize) -> bool {
    text.len() > max_length
}

/// Last 2 path components with `.../` prefix.
///
/// Empty path components (e.g. from `//foo/bar`) are filtered out, which is an
/// intentional improvement over the TypeScript version that did not handle
/// consecutive separators gracefully.
pub fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() <= 2 {
        return path.to_string();
    }
    format!(".../{}", parts[parts.len() - 2..].join("/"))
}

// -------------------------------------------------------- message chunking

/// Options for `chunk_message_with_options`.
pub struct ChunkOptions {
    /// Maximum character length per chunk (default: 4000).
    pub max_length: usize,
    /// Whether to avoid splitting inside triple-backtick code blocks (default: true).
    pub preserve_code_blocks: bool,
    /// Whether to prepend "Part N/M" headers to multi-chunk output (default: true).
    pub add_part_headers: bool,
}

impl Default for ChunkOptions {
    fn default() -> Self {
        Self {
            max_length: 4000,
            preserve_code_blocks: true,
            add_part_headers: true,
        }
    }
}

/// Chunk a message using explicit `ChunkOptions`.
///
/// - Code-block aware: never splits inside ` ``` ` blocks (when `preserve_code_blocks` is true).
/// - Uses natural break points (double newline > single newline > period+space > space).
/// - Adds "Part N/M" headers when multi-chunk (when `add_part_headers` is true).
pub fn chunk_message_with_options(text: &str, opts: &ChunkOptions) -> Vec<String> {
    let max_length = opts.max_length;
    if text.len() <= max_length {
        return vec![text.to_string()];
    }

    let code_blocks = if opts.preserve_code_blocks {
        find_code_blocks(text)
    } else {
        Vec::new()
    };

    let mut chunks = Vec::new();
    let mut remaining = text;
    let mut offset = 0usize;

    while !remaining.is_empty() {
        if remaining.len() <= max_length {
            chunks.push(remaining.to_string());
            break;
        }

        let split = find_best_split_point(remaining, max_length, &code_blocks, offset);
        let chunk = remaining[..split].trim_end().to_string();
        remaining = remaining[split..].trim_start();
        offset += split;
        chunks.push(chunk);
    }

    if opts.add_part_headers && chunks.len() > 1 {
        let total = chunks.len();
        chunks = chunks
            .into_iter()
            .enumerate()
            .map(|(i, c)| format!("\u{1F4C4} *Part {}/{}*\n\n{c}", i + 1, total))
            .collect();
    }

    chunks
}

/// Chunk a message into pieces that fit Telegram's 4096-char limit.
///
/// Convenience wrapper around `chunk_message_with_options` using default options.
pub fn chunk_message(text: &str, max_length: usize) -> Vec<String> {
    chunk_message_with_options(
        text,
        &ChunkOptions {
            max_length,
            ..ChunkOptions::default()
        },
    )
}

/// Strip ANSI codes from `content` and split into Telegram-sized chunks.
///
/// Combines `strip_ansi` and `chunk_message` in a single convenient call.
/// Uses `DEFAULT_MAX_LENGTH` when `max_length` is `None`.
#[allow(dead_code)] // Library API
pub fn format_and_chunk(content: &str, max_length: Option<usize>) -> Vec<String> {
    let cleaned = strip_ansi(content);
    chunk_message(&cleaned, max_length.unwrap_or(DEFAULT_MAX_LENGTH))
}

/// Find triple-backtick code block positions.
struct CodeBlock {
    start: usize,
    end: usize,
}

fn find_code_blocks(text: &str) -> Vec<CodeBlock> {
    static CB_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"```[\s\S]*?```").unwrap());

    CB_RE
        .find_iter(text)
        .map(|m| CodeBlock {
            start: m.start(),
            end: m.end(),
        })
        .collect()
}

fn is_inside_code_block(pos: usize, blocks: &[CodeBlock]) -> bool {
    blocks.iter().any(|b| pos > b.start && pos < b.end)
}

fn find_best_split_point(
    text: &str,
    target: usize,
    blocks: &[CodeBlock],
    global_offset: usize,
) -> usize {
    // If target falls inside a code block, split before or after the block.
    for block in blocks {
        let local_start = block.start.saturating_sub(global_offset);
        let local_end = block.end.saturating_sub(global_offset);
        if target > local_start && target < local_end {
            if local_start > 100 {
                return local_start;
            }
            return std::cmp::min(local_end, text.len());
        }
    }

    // Search for natural break points near target.
    let search_start = target.saturating_sub(200);
    let search_end = std::cmp::min(text.len(), target + 50);
    let search_text = &text[search_start..search_end];

    let break_patterns: &[(&str, usize)] = &[("\n\n", 2), ("\n", 1), (". ", 2), (" ", 1)];

    for &(pat, offset) in break_patterns {
        let mut best: Option<usize> = None;
        for (i, _) in search_text.match_indices(pat) {
            let abs = search_start + i + offset;
            if abs <= target && !is_inside_code_block(abs + global_offset, blocks) {
                best = Some(abs);
            }
        }
        if let Some(b) = best {
            return b;
        }
    }

    // Fallback: split at target.
    target
}

// ===================================================================== tests

#[cfg(test)]
mod tests {
    use super::*;

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
        let result = truncate(emoji, 3);
        // Should not panic and should end with "..."
        assert!(result.ends_with("..."));
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
}
