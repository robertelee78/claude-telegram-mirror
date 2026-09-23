use crate::error::Result;
use crate::types::{is_valid_slash_command, ALLOWED_TMUX_KEYS};
use std::process::Command;

/// How long to wait for typed text to appear in the composer before submitting.
const SETTLE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1500);
/// How long to wait for the composer to empty after Enter.
const SUBMIT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1500);
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);
/// Extra Enters to try when the first one did not submit.
const SUBMIT_RETRIES: u32 = 2;
/// A horizontal rule in Claude Code's TUI: the input box is drawn between two of them.
/// Counting `─` rather than matching the whole line tolerates a label on the rule
/// (Claude Code puts the session name there: `──── agent_comms ─`).
const RULE_MIN_DASHES: usize = 20;

/// A short, distinctive tail of the injected text, used to recognise it on screen.
///
/// The tail rather than the head because a long message scrolls: its end is what sits
/// by the cursor. Whitespace is dropped so the TUI's own wrapping cannot break the
/// comparison, and control characters are ignored for the same reason.
pub(crate) fn submit_marker(text: &str) -> String {
    let squashed: String = text
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect();
    let n = squashed.chars().count();
    squashed.chars().skip(n.saturating_sub(24)).collect()
}

/// Claude Code's input box: the line(s) between the last two horizontal rules.
///
/// `None` when the pane has no such pair — not Claude's TUI, or a layout this does not
/// recognise — in which case "is the text still in the box?" has no answer.
///
/// This replaced "the bottom 14 lines", which produced a false "Reply failed" for
/// messages that *had* been delivered (2026-09-23, 11 of 11 flagged sends were in the
/// agent's transcript). Both places a delivered message is drawn are above the box: a
/// just-sent message is the last line of the conversation, and one sent while the
/// agent is busy is shown as queued (`❯ text` / `ctrl+x ctrl+s to send now`) with the
/// box itself reading `Press up to edit queued messages`. Captured from Claude Code and
/// kept as fixtures (`tests/fixtures/pane-*.txt`).
pub(crate) fn composer_region(pane: &str) -> Option<String> {
    let lines: Vec<&str> = pane.lines().collect();
    let rules: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.chars().filter(|c| *c == '─').count() >= RULE_MIN_DASHES)
        .map(|(i, _)| i)
        .collect();
    let [.., top, bottom] = rules[..] else {
        return None;
    };
    Some(lines[top + 1..bottom].join(""))
}

/// Is `marker` still in the input box? `None` when the box cannot be located.
pub(crate) fn composer_contains(pane: &str, marker: &str) -> Option<bool> {
    if marker.is_empty() {
        return None;
    }
    let region: String = composer_region(pane)?
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect();
    Some(region.contains(marker))
}

/// Input injector for sending user input from Telegram to Claude Code CLI via tmux.
///
/// Security: ALL tmux commands use Command::arg() — NO shell interpolation.
/// This prevents command injection via user-controlled inputs like session names,
/// socket paths, or message text.
///
/// ROUTING-001 (stateless routing): this type holds **no** tmux target/socket
/// state. Every action method takes the `(target, socket)` pair explicitly, so a
/// single shared `InputInjector` (behind a `Mutex` whose only job is to serialize
/// tmux command execution) can never bleed one session's pane onto another's
/// injection. The previous design stored a mutable `tmux_target` that callers set
/// in one critical section and read in another — a TOCTOU race that misrouted a
/// reply meant for session B into session A's pane when handlers ran concurrently
/// (each Telegram update is `tokio::spawn`-ed). Passing the target per call
/// eliminates that class of bug entirely.
pub struct InputInjector;

/// STALE-TOPICS: What `InputInjector::pane_claude_state` observed running in a pane.
/// See that method for the precise meaning of each variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneClaudeState {
    /// The pane title contains "Claude Code" — Claude's TUI is in the foreground.
    RunningClaude,
    /// The foreground command is a known shell — Claude has exited to a prompt.
    Shell,
    /// Neither could be confirmed (e.g. an editor, or tmux not queryable).
    Unknown,
}

/// STALE-TOPICS: Does `cmd` name a known interactive shell? Used to detect that a
/// Claude pane has fallen back to a shell prompt. tmux reports `pane_current_command`
/// as the bare program name; a login shell may be prefixed with `-` (e.g. `-zsh`).
fn is_shell_command(cmd: &str) -> bool {
    let c = cmd.trim().trim_start_matches('-').to_ascii_lowercase();
    matches!(
        c.as_str(),
        "zsh" | "bash" | "sh" | "fish" | "dash" | "ksh" | "tcsh" | "csh" | "ash" | "login"
    )
}

impl Default for InputInjector {
    fn default() -> Self {
        Self::new()
    }
}

impl InputInjector {
    pub fn new() -> Self {
        Self
    }

    /// Validate + sanitize a tmux socket path, rejecting traversal / non-absolute /
    /// oversized paths. A rejected socket degrades to `None` (default tmux server),
    /// matching the historical `set_target` behavior. Centralized here so every
    /// stateless entry point applies the same guard.
    fn sanitized_socket(socket: Option<&str>) -> Option<String> {
        socket.and_then(|s| {
            if s.contains("..") {
                tracing::warn!(path = %s, "Rejecting tmux socket path with '..' traversal");
                return None;
            }
            if !s.starts_with('/') {
                tracing::warn!(path = %s, "Rejecting non-absolute tmux socket path");
                return None;
            }
            if s.len() > 256 {
                tracing::warn!(len = s.len(), "Rejecting oversized tmux socket path");
                return None;
            }
            Some(s.to_string())
        })
    }

    /// Build the `-S <socket>` args for a tmux command from a sanitized socket.
    fn socket_args(socket: &Option<String>) -> Vec<&str> {
        match socket {
            Some(s) => vec!["-S", s.as_str()],
            None => vec![],
        }
    }

    /// Validate that the tmux target pane exists (BUG-001 fix).
    pub fn validate_target(target: &str, socket: Option<&str>) -> std::result::Result<(), String> {
        let socket = Self::sanitized_socket(socket);
        let mut cmd = Command::new("tmux");
        for arg in Self::socket_args(&socket) {
            cmd.arg(arg);
        }
        cmd.arg("list-panes").arg("-t").arg(target);

        match cmd.output() {
            Ok(output) if output.status.success() => Ok(()),
            _ => Err(format!(
                "Pane \"{}\" not found. Claude may have moved to a different pane. \
                 Send any command in Claude to refresh the connection.",
                target
            )),
        }
    }

    /// Inject text input into the given tmux pane (literal text + trailing Enter).
    /// Uses Command::arg() — no shell interpolation possible.
    /// Type `text` into the pane and submit it.
    ///
    /// The naive form of this — send the text, immediately send Enter — races the TUI's
    /// input handling: for a long message the Enter can arrive while the pane is still
    /// ingesting, and it is swallowed. The operator then finds their message sitting in
    /// the composer, unsent, and has to press Enter at the console (reported for a
    /// Claude Code session; the message was long).
    ///
    /// So this does what ADR-015 established for the question widget: read the pane and
    /// act on what is actually there. Type, wait until the text appears in the composer,
    /// press Enter, then confirm the composer emptied — retrying Enter a couple of times
    /// if it did not. When the pane cannot be read at all it falls back to the old
    /// behaviour, which is never worse than before.
    pub fn inject(&self, target: &str, socket: Option<&str>, text: &str) -> Result<bool> {
        if let Err(reason) = Self::validate_target(target, socket) {
            tracing::warn!(%reason, "Target validation failed");
            return Ok(false);
        }
        let socket_owned = Self::sanitized_socket(socket);

        // Send text with -l (literal mode)
        let mut send_cmd = Command::new("tmux");
        for arg in Self::socket_args(&socket_owned) {
            send_cmd.arg(arg);
        }
        send_cmd
            .arg("send-keys")
            .arg("-t")
            .arg(target)
            .arg("-l")
            .arg(text);

        let output = send_cmd.output()?;
        if !output.status.success() {
            tracing::error!(
                stderr = %String::from_utf8_lossy(&output.stderr),
                "tmux send-keys failed"
            );
            return Ok(false);
        }

        let marker = submit_marker(text);

        // Wait for the typed text to land in the composer before pressing Enter.
        if !marker.is_empty() {
            // Unlocatable box: nothing to wait for, go ahead.
            self.wait_until(target, socket, SETTLE_TIMEOUT, |pane| {
                composer_contains(pane, &marker).unwrap_or(true)
            });
        }

        for attempt in 0..=SUBMIT_RETRIES {
            let mut enter_cmd = Command::new("tmux");
            for arg in Self::socket_args(&socket_owned) {
                enter_cmd.arg(arg);
            }
            enter_cmd
                .arg("send-keys")
                .arg("-t")
                .arg(target)
                .arg("Enter");
            let output = enter_cmd.output()?;
            if !output.status.success() {
                tracing::error!(
                    stderr = %String::from_utf8_lossy(&output.stderr),
                    "tmux send-keys Enter failed"
                );
                return Ok(false);
            }

            // Nothing to verify against (empty or unreadable pane): keep the old
            // fire-and-forget behaviour.
            if marker.is_empty() {
                break;
            }
            // Submitted = the text has left the input box. An unlocatable box is not
            // evidence of failure: report success, as before this check existed,
            // rather than a false "Reply failed".
            let submitted = self.wait_until(target, socket, SUBMIT_TIMEOUT, |pane| {
                composer_contains(pane, &marker) != Some(true)
            });
            if submitted {
                if attempt > 0 {
                    tracing::info!(%target, attempt, "Submit needed a repeated Enter");
                }
                break;
            }
            if attempt == SUBMIT_RETRIES {
                tracing::warn!(
                    %target,
                    "Text is in the composer but would not submit after {} attempts",
                    SUBMIT_RETRIES + 1
                );
                return Ok(false);
            }
        }

        tracing::debug!(%target, text_len = text.len(), "Injected text via tmux");
        Ok(true)
    }

    /// Poll the pane until `pred` holds, or the budget runs out. `false` when the pane
    /// cannot be read (the caller then proceeds without verification).
    fn wait_until(
        &self,
        target: &str,
        socket: Option<&str>,
        budget: std::time::Duration,
        pred: impl Fn(&str) -> bool,
    ) -> bool {
        let deadline = std::time::Instant::now() + budget;
        loop {
            match self.capture_pane(target, socket) {
                Some(pane) if pred(&pane) => return true,
                Some(_) => {}
                None => return false,
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    /// ADR-015: Inject LITERAL text into the tmux pane WITHOUT a trailing Enter.
    ///
    /// Unlike [`inject`], this sends only the characters (`send-keys -t <target> -l <text>`)
    /// and does NOT press Enter afterward. It is used to type into the AskUserQuestion
    /// widget's `Type something` free-text row, where the surrounding inject_answers flow
    /// owns the subsequent commit/advance keystroke (single-select free-text commits with
    /// one Enter; multi-select free-text advances via the `Next`/`Submit` row). Reusing
    /// the Enter-appending [`inject`] here would double-fire and submit prematurely.
    ///
    /// `Command::arg()` only — no shell interpolation. Callers MUST sanitize/cap the text
    /// (strip control chars/newlines, bound length) before calling. Returns `Ok(false)` on
    /// a missing target or tmux soft failure, `Err` on a hard failure.
    pub fn inject_literal(&self, target: &str, socket: Option<&str>, text: &str) -> Result<bool> {
        if let Err(reason) = Self::validate_target(target, socket) {
            tracing::warn!(%reason, "Target validation failed (inject_literal)");
            return Ok(false);
        }
        let socket = Self::sanitized_socket(socket);

        let mut send_cmd = Command::new("tmux");
        for arg in Self::socket_args(&socket) {
            send_cmd.arg(arg);
        }
        send_cmd
            .arg("send-keys")
            .arg("-t")
            .arg(target)
            .arg("-l")
            .arg(text);

        let output = send_cmd.output()?;
        if !output.status.success() {
            tracing::error!(
                stderr = %String::from_utf8_lossy(&output.stderr),
                "tmux send-keys -l (literal, no Enter) failed"
            );
            return Ok(false);
        }

        tracing::debug!(%target, text_len = text.len(), "Injected literal text (no Enter) via tmux");
        Ok(true)
    }

    /// Send a special key (from whitelist only).
    /// BUG-004 fix: includes socket flag for correct tmux server targeting.
    pub fn send_key(&self, target: &str, socket: Option<&str>, key: &str) -> Result<bool> {
        let tmux_key = match key {
            "Enter" => "Enter",
            "Escape" => "Escape",
            "Tab" => "Tab",
            "Ctrl-C" => "C-c",
            "Ctrl-U" => "C-u",
            "Ctrl-D" => "C-d",
            "Ctrl-L" => "C-l",
            _ => key,
        };

        if !ALLOWED_TMUX_KEYS.contains(&tmux_key) {
            tracing::warn!(key = %tmux_key, "Key not in whitelist, rejecting");
            return Ok(false);
        }

        let socket = Self::sanitized_socket(socket);
        let mut cmd = Command::new("tmux");
        for arg in Self::socket_args(&socket) {
            cmd.arg(arg);
        }
        cmd.arg("send-keys").arg("-t").arg(target).arg(tmux_key);

        let output = cmd.output()?;
        if !output.status.success() {
            tracing::error!(
                stderr = %String::from_utf8_lossy(&output.stderr),
                "tmux send-keys failed for key"
            );
            return Ok(false);
        }

        tracing::debug!(%target, %key, "Sent key via tmux");
        Ok(true)
    }

    /// ADR-015: Capture the visible text of the configured tmux pane.
    ///
    /// Read-only (`tmux capture-pane -t <target> -p`); reuses the validated target and
    /// socket. `Command::arg()` only — NO shell interpolation. Returns the pane's
    /// visible text, or `None` if no target is set / capture fails. Used by the submit
    /// flow's readiness detection (poll until Claude's multi-select / review screen has
    /// rendered) instead of blind sleeps.
    pub fn capture_pane(&self, target: &str, socket: Option<&str>) -> Option<String> {
        let socket = Self::sanitized_socket(socket);
        let mut cmd = Command::new("tmux");
        for arg in Self::socket_args(&socket) {
            cmd.arg(arg);
        }
        cmd.arg("capture-pane").arg("-t").arg(target).arg("-p");

        match cmd.output() {
            Ok(output) if output.status.success() => {
                Some(String::from_utf8_lossy(&output.stdout).into_owned())
            }
            Ok(output) => {
                tracing::debug!(
                    stderr = %String::from_utf8_lossy(&output.stderr),
                    "tmux capture-pane returned non-zero status"
                );
                None
            }
            Err(e) => {
                tracing::debug!(error = %e, "tmux capture-pane failed to spawn");
                None
            }
        }
    }

    /// Send a slash command (like /clear).
    /// Validates against character whitelist, sends with -l flag.
    pub fn send_slash_command(
        &self,
        target: &str,
        socket: Option<&str>,
        command: &str,
    ) -> Result<bool> {
        if !is_valid_slash_command(command) {
            tracing::warn!(%command, "Slash command rejected: contains unsafe characters");
            return Ok(false);
        }

        let socket = Self::sanitized_socket(socket);
        // Send command text with -l (literal mode)
        let mut cmd = Command::new("tmux");
        for arg in Self::socket_args(&socket) {
            cmd.arg(arg);
        }
        cmd.arg("send-keys")
            .arg("-t")
            .arg(target)
            .arg("-l")
            .arg(command);

        let output = cmd.output()?;
        if !output.status.success() {
            return Ok(false);
        }

        // Send Enter
        let mut enter_cmd = Command::new("tmux");
        for arg in Self::socket_args(&socket) {
            enter_cmd.arg(arg);
        }
        enter_cmd
            .arg("send-keys")
            .arg("-t")
            .arg(target)
            .arg("Enter");

        let output = enter_cmd.output()?;
        Ok(output.status.success())
    }

    /// Check if tmux is available
    #[allow(dead_code)] // Library API
    pub fn is_tmux_available() -> bool {
        Command::new("tmux")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Check if a tmux pane is alive
    pub fn is_pane_alive(target: &str, socket: Option<&str>) -> bool {
        let mut cmd = Command::new("tmux");
        if let Some(s) = socket {
            cmd.arg("-S").arg(s);
        }
        cmd.arg("list-panes").arg("-t").arg(target);
        cmd.output().map(|o| o.status.success()).unwrap_or(false)
    }

    /// STALE-TOPICS: Classify what is running in a (presumed-alive) tmux pane, so the
    /// daemon can tell whether the *specific Claude session* it routed there is still
    /// alive — the pane being alive is necessary but not sufficient (a pane outlives
    /// its Claude session when the user exits Claude back to a shell prompt).
    ///
    /// A single `tmux display-message` reads the pane title and foreground command:
    ///   - `RunningClaude` — the pane title contains "Claude Code" (the TUI sets the
    ///     terminal title to "✳ Claude Code", with spinner-glyph variants). This is the
    ///     confident-alive signal; such a pane is NEVER pruned, regardless of idle time.
    ///   - `Shell` — the foreground command is a known shell (zsh/bash/…), i.e. Claude
    ///     exited and the pane fell back to a prompt. Confident-dead → prune.
    ///   - `Unknown` — neither (e.g. an editor left open). We do NOT prune on this alone
    ///     (avoids deleting a live session's topic on a flaky title); the inactivity
    ///     backstop handles it only after the long threshold.
    ///
    /// Returns `Unknown` if tmux cannot be queried (the caller has already established
    /// pane liveness separately via `is_pane_alive`).
    pub fn pane_claude_state(target: &str, socket: Option<&str>) -> PaneClaudeState {
        // Unit separator (0x1f) cannot appear in a tmux command name and is vanishingly
        // unlikely in a pane title — a safe field delimiter for the format string.
        let mut cmd = Command::new("tmux");
        if let Some(s) = socket {
            cmd.arg("-S").arg(s);
        }
        cmd.arg("display-message")
            .arg("-p")
            .arg("-t")
            .arg(target)
            .arg("-F")
            .arg("#{pane_title}\u{1f}#{pane_current_command}");

        let output = match cmd.output() {
            Ok(o) if o.status.success() => o,
            _ => return PaneClaudeState::Unknown,
        };
        let raw = String::from_utf8_lossy(&output.stdout);
        let line = raw.trim_end_matches(['\n', '\r']);
        let (title, command) = line.split_once('\u{1f}').unwrap_or(("", line));

        // Confident-alive: the Claude Code TUI owns this pane.
        if title.contains("Claude Code") {
            return PaneClaudeState::RunningClaude;
        }
        // Confident-dead: Claude exited and the pane is back at a shell prompt.
        if is_shell_command(command) {
            return PaneClaudeState::Shell;
        }
        PaneClaudeState::Unknown
    }

    /// ROUTING-002: Build `TmuxInfo` from a stable tmux pane id (`$TMUX_PANE`).
    ///
    /// Pure and unit-testable: it neither reads process-global env nor spawns
    /// tmux. Returns `None` when `tmux_pane` is absent/empty, signalling the
    /// caller to fall back to positional detection. The pane id (e.g. `%24`) is
    /// stored verbatim as the routing `target`; it is a first-class tmux target
    /// accepted by send-keys/capture-pane/list-panes/display-message alike.
    fn tmux_info_from_pane_id(socket: Option<String>, tmux_pane: Option<&str>) -> Option<TmuxInfo> {
        let pane_id = tmux_pane?.trim();
        if pane_id.is_empty() {
            return None;
        }
        Some(TmuxInfo {
            // The positional `session`/`pane` indices are not meaningful when we
            // route by pane id; only `target`/`socket` are consumed downstream.
            session: String::new(),
            pane: pane_id.to_string(),
            target: pane_id.to_string(),
            socket,
        })
    }

    /// Detect the current tmux pane from the environment.
    ///
    /// ROUTING-002: The pane is identified by its STABLE tmux pane id
    /// (`$TMUX_PANE`, e.g. `%24`), read from the environment the hook process
    /// inherited from its own pane. This is the only correct source:
    ///
    ///   - tmux sets `$TMUX_PANE` per-pane and it is inherited by every child
    ///     process (Claude Code and the hooks it spawns), so it always names the
    ///     pane the hook actually ran in. Pane ids are unique and unchanged for
    ///     the life of the pane within the server (tmux(1)).
    ///   - The previous implementation built a POSITIONAL target
    ///     (`session:window.pane`) from bare `tmux display-message -p '#S'/'#I'/'#P'`
    ///     calls. With no `-t`, tmux resolves those against the attached client's
    ///     ACTIVE pane — NOT the calling pane (tmux(1): "otherwise the active
    ///     pane"). With multiple Claude sessions in one server, every session's
    ///     hook then recorded whichever pane the user was looking at, so all
    ///     sessions' targets collapsed onto the active pane and Telegram→CLI
    ///     replies misrouted. Positional ids are also unstable (reused on pane
    ///     renumber/reorder). tmux(1) explicitly recommends pane/window/session
    ///     IDs over positional targets "from a script".
    ///
    /// Falls back to the legacy positional detection only if `$TMUX_PANE` is
    /// unset though `$TMUX` is set (not expected in normal tmux).
    pub fn detect_tmux_session() -> Option<TmuxInfo> {
        let tmux_env = std::env::var("TMUX").ok()?;
        let socket_path = tmux_env.split(',').next().map(|s| s.to_string());

        // Preferred path: the hook's own pane id, straight from the inherited env.
        if let Some(info) = Self::tmux_info_from_pane_id(
            socket_path.clone(),
            std::env::var("TMUX_PANE").ok().as_deref(),
        ) {
            return Some(info);
        }

        // Defensive fallback: $TMUX set but $TMUX_PANE unset. Derive a positional
        // target from the active pane. This is the drift-prone legacy path, used
        // only when the stable pane id is unavailable.
        tracing::warn!(
            "TMUX_PANE unset though TMUX is set; falling back to positional tmux \
             target (may misroute with multiple concurrent sessions)"
        );
        let query = |fmt: &str| -> Option<String> {
            Command::new("tmux")
                .arg("display-message")
                .arg("-p")
                .arg(fmt)
                .output()
                .ok()
                .and_then(|o| {
                    o.status
                        .success()
                        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
                })
        };
        let session = query("#S")?;
        let window = query("#I").unwrap_or_else(|| "0".to_string());
        let pane = query("#P").unwrap_or_else(|| "0".to_string());
        let target = format!("{}:{}.{}", session, window, pane);

        Some(TmuxInfo {
            session,
            pane,
            target,
            socket: socket_path,
        })
    }

    // ROUTING-001: `find_claude_code_session()` was removed. It scanned for the
    // FIRST tmux pane running a "claude" process and returned only its session
    // name (caller manufactured `"{name}:0.0"` — pane 0). Using it for per-session
    // routing bound a session with a missing pane mapping to an unrelated pane 0,
    // causing cross-session misrouting. There is no safe way to recover a specific
    // session's pane by guessing; the only trusted source is the Claude hook's
    // `tmuxTarget` recorded at session_start.
}

/// L3.6: Escape text for tmux send-keys.
///
/// DEPRECATED: With `Command::arg()` and `-l` flag, no escaping is needed.
/// This function is retained for API compatibility with external callers.
#[deprecated(note = "Not needed with Command::arg() + -l flag")]
#[allow(dead_code)] // Library API (deprecated)
pub fn escape_tmux_text(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Get hostname safely
pub fn get_hostname() -> String {
    Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone)]
pub struct TmuxInfo {
    #[allow(dead_code)] // Populated by detect_tmux_session; part of the struct's shape
    pub session: String,
    #[allow(dead_code)] // Library API
    pub pane: String,
    pub target: String,
    pub socket: Option<String>,
}

#[cfg(test)]
mod tests {

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn the_marker_is_the_tail_and_survives_the_tuis_wrapping() {
        let text = "1) explain this part better. 7) what do you suggest? Again I want this to be simple ux for a user, like a tor hidden service.";
        let m = submit_marker(text);
        assert!(!m.is_empty());
        assert!(text.replace(' ', "").ends_with(&m), "it is the tail: {m}");
        // Wrapped across several lines inside the box still matches.
        let rule = "─".repeat(80);
        let wrapped = format!(
            "earlier output\n{rule}\n❯ 1) explain this part better. 7) what do you suggest? Again I want\n  this to be simple ux for a user, like a tor hidden service.\n{rule}\n  status line\n"
        );
        assert_eq!(composer_contains(&wrapped, &m), Some(true));
    }

    #[test]
    fn a_message_sent_while_the_agent_is_idle_has_left_the_box() {
        // Captured from Claude Code right after a submit: the message is the last line
        // of the conversation, inside the bottom 14 lines — which is what made the old
        // check report "Reply failed" for a delivered message.
        let pane = fixture("pane-idle-after-submit.txt");
        let m = submit_marker("QUEUED_WHILE_BUSY please also say hello");
        assert!(
            pane.contains("QUEUED_WHILE_BUSY"),
            "precondition: visible on screen"
        );
        assert_eq!(composer_contains(&pane, &m), Some(false));
    }

    #[test]
    fn a_message_queued_while_the_agent_is_busy_has_left_the_box() {
        // 2026-09-23 10:52–10:59: nine replies flagged "would not submit", all nine in
        // the agent's transcript as queued commands. This is how Claude Code draws them.
        let pane = fixture("pane-queued-while-busy.txt");
        let m = submit_marker("QUEUED_TWO say hi after");
        assert!(
            pane.contains("ctrl+x ctrl+s to send now"),
            "precondition: queued"
        );
        assert_eq!(composer_contains(&pane, &m), Some(false));
    }

    #[test]
    fn text_really_stuck_in_the_box_is_still_caught() {
        let pane = fixture("pane-text-in-composer.txt");
        let m = submit_marker("STILL_IN_THE_BOX not sent");
        assert_eq!(composer_contains(&pane, &m), Some(true));
        // The real agent_comms pane on 2026-09-23 with unsent text in its box.
        let rule = "─".repeat(120);
        let real = format!(
            "✻ Sautéed for 34s · done 10:02 PM\n\n{rule}\n❯ wait, fix it properly\n{rule}\n  ▊ RuFlo V3.44.0 ● vox\n  ⏵⏵ bypass permissions on\n"
        );
        assert_eq!(
            composer_contains(&real, &submit_marker("wait, fix it properly")),
            Some(true)
        );
    }

    #[test]
    fn a_labelled_rule_still_counts() {
        // Claude Code puts the session name on the rule above the box.
        let pane = format!(
            "{} agent_comms ─\n❯ hello there\n{}\n",
            "─".repeat(60),
            "─".repeat(60)
        );
        assert_eq!(
            composer_contains(&pane, &submit_marker("hello there")),
            Some(true)
        );
    }

    #[test]
    fn no_recognisable_box_means_no_verdict() {
        // Not Claude's TUI (or a layout this does not know): never a false alarm.
        assert_eq!(
            composer_contains("$ some shell\n$ ", &submit_marker("hi")),
            None
        );
        assert_eq!(submit_marker("   \n\t "), "");
        assert_eq!(composer_contains("anything at all", ""), None);
    }

    use super::*;
    use crate::types::is_valid_slash_command;

    #[test]
    fn test_key_whitelist() {
        assert!(ALLOWED_TMUX_KEYS.contains(&"Enter"));
        assert!(ALLOWED_TMUX_KEYS.contains(&"Escape"));
        assert!(ALLOWED_TMUX_KEYS.contains(&"C-c"));
        assert!(!ALLOWED_TMUX_KEYS.contains(&"rm -rf /"));
    }

    #[test]
    fn test_injector_nonexistent_target() {
        // A target that does not exist fails validation and injects nothing
        // (returns Ok(false)) rather than misrouting.
        let injector = InputInjector::new();
        assert!(!injector
            .inject("ctm-no-such-session:9.9", None, "test")
            .unwrap());
    }

    #[test]
    fn test_socket_path_sanitization() {
        // Valid absolute path is preserved.
        assert_eq!(
            InputInjector::sanitized_socket(Some("/tmp/tmux-1000/default")).as_deref(),
            Some("/tmp/tmux-1000/default")
        );
        // Path traversal rejected -> None (degrades to default server).
        assert!(InputInjector::sanitized_socket(Some("/tmp/../etc/evil")).is_none());
        // Relative path rejected.
        assert!(InputInjector::sanitized_socket(Some("relative/path")).is_none());
        // Oversized path rejected.
        let long = format!("/{}", "a".repeat(256));
        assert!(InputInjector::sanitized_socket(Some(&long)).is_none());
    }

    #[test]
    fn test_slash_command_validation() {
        assert!(is_valid_slash_command("/clear"));
        assert!(is_valid_slash_command("/rename My Feature"));
        assert!(!is_valid_slash_command("/clear;rm -rf /"));
        assert!(!is_valid_slash_command(""));
    }

    #[test]
    fn test_tmux_info_from_pane_id() {
        // ROUTING-002: the stable pane id is stored verbatim as the target, and
        // the socket is threaded through untouched.
        let info = InputInjector::tmux_info_from_pane_id(
            Some("/tmp/tmux-1000/default".to_string()),
            Some("%24"),
        )
        .expect("pane id present -> Some");
        assert_eq!(info.target, "%24");
        assert_eq!(info.pane, "%24");
        assert_eq!(info.socket.as_deref(), Some("/tmp/tmux-1000/default"));

        // Surrounding whitespace from `display-message`/env is trimmed.
        let trimmed = InputInjector::tmux_info_from_pane_id(None, Some("  %7\n"))
            .expect("trimmed pane id -> Some");
        assert_eq!(trimmed.target, "%7");

        // Absent or empty pane id -> None, so the caller uses the positional fallback.
        assert!(InputInjector::tmux_info_from_pane_id(None, None).is_none());
        assert!(InputInjector::tmux_info_from_pane_id(None, Some("")).is_none());
        assert!(InputInjector::tmux_info_from_pane_id(None, Some("   ")).is_none());
    }

    #[test]
    fn test_is_shell_command() {
        // STALE-TOPICS: a pane back at a shell prompt means Claude exited → prunable.
        for shell in [
            "zsh", "bash", "sh", "fish", "dash", "ksh", "-zsh", "-bash", "  ZSH  ",
        ] {
            assert!(is_shell_command(shell), "{shell:?} should be a shell");
        }
        // Claude renames its arg0 to a version string; never a shell. Editors/other
        // programs must NOT be treated as a shell (kept Unknown, never falsely pruned).
        for not_shell in [
            "2.1.181", "claude", "node", "vim", "nvim", "less", "python", "",
        ] {
            assert!(
                !is_shell_command(not_shell),
                "{not_shell:?} should not be a shell"
            );
        }
    }
}
