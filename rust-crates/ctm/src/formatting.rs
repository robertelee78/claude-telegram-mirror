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
///
/// If `text` has more than `max_len` characters, it is truncated and an
/// ellipsis (`...`) is appended. The returned string is at most `max_len`
/// characters long.
///
/// # Examples
///
/// ```
/// use ctm::formatting::truncate;
///
/// // Short strings are returned unchanged.
/// assert_eq!(truncate("hello", 10), "hello");
///
/// // Long strings are truncated with an ellipsis.
/// assert_eq!(truncate("hello world", 8), "hello...");
/// ```
pub fn truncate(text: &str, max_len: usize) -> String {
    if max_len < 4 {
        // Below minimum useful length — return what fits without ellipsis.
        return text.chars().take(max_len).collect();
    }
    let char_count = text.chars().count();
    if char_count <= max_len {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_len - 3).collect();
    format!("{truncated}...")
}

/// Estimate the number of chunks needed to fit `text` within `max_length`.
///
/// Uses character count (not byte length) for consistency with `truncate()`
/// and Telegram's character-based message limits.
#[allow(dead_code)] // Library API
pub fn estimate_chunks(text: &str, max_length: usize) -> usize {
    let char_count = text.chars().count();
    if char_count <= max_length {
        1
    } else {
        char_count.div_ceil(max_length)
    }
}

/// Returns `true` if `text` exceeds `max_length` characters and will need chunking.
///
/// Uses character count (not byte length) for consistency with `truncate()`
/// and Telegram's character-based message limits.
#[allow(dead_code)] // Library API
pub fn needs_chunking(text: &str, max_length: usize) -> bool {
    text.chars().count() > max_length
}

/// Last 2 path components with `.../` prefix.
///
/// Empty path components (e.g. from `//foo/bar`) are filtered out, which is an
/// intentional improvement over the TypeScript version that did not handle
/// consecutive separators gracefully.
///
/// # Examples
///
/// ```
/// use ctm::formatting::short_path;
///
/// // Long paths are shortened to the last two components.
/// assert_eq!(short_path("/opt/project/src/utils/config.ts"), ".../utils/config.ts");
///
/// // Paths with two or fewer components are returned as-is.
/// assert_eq!(short_path("/src/file.ts"), "/src/file.ts");
/// ```
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

    // Bug 1 fix: use character count, not byte length.
    if text.chars().count() <= max_length {
        return vec![text.to_string()];
    }

    let code_blocks = if opts.preserve_code_blocks {
        find_code_blocks(text)
    } else {
        Vec::new()
    };

    // Bug 3 fix: reserve space for part headers BEFORE splitting so that
    // the header does not push any chunk over max_length.
    // Header format: "📄 *Part N/M*\n\n" — we conservatively reserve 30 chars.
    // We only apply the overhead when headers are actually requested, and we
    // do a two-pass approach: first split with effective_max, then add headers.
    let header_overhead: usize = if opts.add_part_headers { 30 } else { 0 };
    let effective_max = max_length.saturating_sub(header_overhead);

    let mut chunks: Vec<String> = Vec::new();
    let mut remaining = text;
    let mut offset = 0usize;

    while !remaining.is_empty() {
        // Bug 1 fix: use character count, not byte length.
        if remaining.chars().count() <= effective_max {
            chunks.push(remaining.to_string());
            break;
        }

        let split = find_best_split_point(remaining, effective_max, &code_blocks, offset);
        let chunk = remaining[..split].trim_end().to_string();

        // Bug 4 fix: account for bytes consumed by trim_start() when advancing offset.
        let after_split = &remaining[split..];
        let trimmed = after_split.trim_start();
        let trim_bytes = after_split.len() - trimmed.len();
        remaining = trimmed;
        offset += split + trim_bytes;

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

/// Convert a character position to a byte position within `text`.
///
/// Returns the byte index at which the character at position `char_pos` starts.
/// If `char_pos` is beyond the end of `text`, returns `text.len()`.
fn char_pos_to_byte_pos(text: &str, char_pos: usize) -> usize {
    text.char_indices()
        .nth(char_pos)
        .map(|(byte_idx, _)| byte_idx)
        .unwrap_or(text.len())
}

/// Find the best byte-offset split point in `text` near the given `target`
/// character position.
///
/// - `target`        : desired split position in **characters**.
/// - `blocks`        : code-block byte ranges in the full original text.
/// - `global_offset` : byte offset of `text` within the original full text.
///
/// Returns a **byte** index into `text` that is guaranteed to be on a UTF-8
/// character boundary and safe to use for slicing.
fn find_best_split_point(
    text: &str,
    target: usize,
    blocks: &[CodeBlock],
    global_offset: usize,
) -> usize {
    // Convert target character position to a byte position in `text`.
    let target_byte = char_pos_to_byte_pos(text, target);

    // If target_byte falls inside a code block, split before or after the block.
    for block in blocks {
        let local_start = block.start.saturating_sub(global_offset);
        let local_end = block.end.saturating_sub(global_offset);
        if target_byte > local_start && target_byte < local_end {
            if local_start > 100 {
                return local_start;
            }
            return std::cmp::min(local_end, text.len());
        }
    }

    // Search for natural break points near target_byte, but only at char boundaries.
    // We restrict the search window to [search_start_byte .. search_end_byte].
    let search_start_char = target.saturating_sub(200);
    let search_start_byte = char_pos_to_byte_pos(text, search_start_char);
    let search_end_byte = std::cmp::min(text.len(), char_pos_to_byte_pos(text, target + 50));

    // Guard: search window must be valid.
    if search_start_byte >= search_end_byte || search_end_byte > text.len() {
        return target_byte;
    }

    let search_text = &text[search_start_byte..search_end_byte];

    // Break patterns ordered by preference.  `pat_len` is the byte length of
    // the portion we want to include *before* the split (i.e. we split after
    // the delimiter, consuming `after_pat` bytes of the delimiter).
    let break_patterns: &[(&str, usize)] = &[("\n\n", 2), ("\n", 1), (". ", 2), (" ", 1)];

    for &(pat, after_pat) in break_patterns {
        let mut best: Option<usize> = None;
        for (i, _) in search_text.match_indices(pat) {
            // `i` is a byte offset within `search_text`, which is a slice of
            // `text` starting at `search_start_byte`.
            let abs_byte = search_start_byte + i + after_pat;
            // Only accept positions at or before target_byte so we never
            // exceed the character budget.
            if abs_byte <= target_byte && !is_inside_code_block(abs_byte + global_offset, blocks) {
                best = Some(abs_byte);
            }
        }
        if let Some(b) = best {
            return b;
        }
    }

    // Fallback: split exactly at the target character boundary (byte-safe).
    target_byte
}
